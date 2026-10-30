#!/usr/bin/env python3
"""The nine checks of check-fork-push.sh over exactly what one push would publish.

Run by check-fork-push.sh, which builds the snapshot commit and the private work directory; see
docs/node-program/designs/CI-FORK.md. Output names a check and a pushed path (or "commit message",
"ref name"), never a denylist term or private content. Exit status: 0 when no check fails (check 3
only warns), 1 when one fails, 2 on a usage or setup error.

Subcommands: check (the nine checks), rewrite (the snapshot's export-only source rewrite),
install-gitleaks (the pinned release).
"""
import argparse
import base64
import bz2
import concurrent.futures
import fnmatch
import hashlib
import io
import json
import lzma
import multiprocessing
import os
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import tomllib
import unicodedata
import urllib.request
import zipfile
import zlib

import yaml

try:
    from compression import zstd
except ImportError:  # Python < 3.14: checks 1 and 2 refuse to run (they cannot see zstd content)
    zstd = None

POINTER = re.compile(rb"EMBARGO-[0-9]|CANDIDATE-[A-Za-z0-9]|DASH_EMBARGO_DIR|private/embargo")
# Local path classes; an allowlist entry records how many of each a file names.
LOCAL_PATHS = {
    "chains": re.compile(rb"/work/chains\b"),
    "bench": re.compile(rb"/work/bench"),
    "cache": re.compile(rb"/work/cache\b"),
    "private": re.compile(rb"~/private\b|\$\{?HOME\}?/private\b|/(home|Users)/[^/\s]+/private\b"),
}
DOC = re.compile(r"(^|/)docs/|\.(md|markdown|rst|adoc)$|(^|/)(LICENSE|COPYING)[^/]*$")
PUBLIC_GIT = re.compile(r"^https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+?(\.git)?/?$")
DATADIR_NAME = re.compile(
    r"(^|/)(blocks/(blk|rev)[0-9]{5}\.dat|chainstate/|evodb/|llmq/|indexes/|"
    r"(wallet|peers|mempool|fee_estimates|banlist|mncache|netfulfilled|governance|sporks)\.dat|"
    r"[0-9]{6}\.(ldb|sst)|MANIFEST-[0-9]{6}|.*\.(sqlite3?|mdb|db))$"
)
NETWORK_MAGICS = (bytes.fromhex("bf0c6bbd"), bytes.fromhex("cee2caff"), bytes.fromhex("fcc1b7dc"))
LEVELDB_FOOTER = bytes.fromhex("57fb808b247547db")
ROCKSDB_FOOTER = bytes.fromhex("f7cff485b741e288")

GITLEAKS_VERSION = "8.30.1"
GITLEAKS_TAR_SHA256 = "551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb"
GITLEAKS_SHA256 = "88f91962aa2f93ac6ab281d553b9e125f5197bbbce38f9f2437f7299c32e5509"

# Decompression (checks 1 and 2): formats, nesting depth and size caps. Hitting a cap fails.
DECODE_DEPTH = 3
DECODE_CAP = 2 << 30  # per blob, all layers together
DECODE_TOTAL_CAP = 3 << 30  # all blobs; fork-push-fp's spool holds under 4 GiB
DECODE_STEP = 64 << 20
NESTED_CAP = 256 << 20  # a layer is opened again only up to this size
CHUNK = 16 << 20  # check 1 reads decoded files in chunks overlapping by OVERLAP
OVERLAP = 64 << 10

# The allowlist's reason codes, per check. `arg` reasons take ":<value>".
REASONS = {
    5: {"local-tool", "nested-workspace", "inert-literal", "absent-skip", "env-gated", "feature-gated"},
    6: {"public-data", "synthetic-data"},
}
GLOB_REASONS = {"local-tool", "nested-workspace"}
ARG_REASONS = {"env-gated", "feature-gated"}

FAILED = []


def fail(check, what):
    FAILED.append((check, what))
    print(f"check-fork-push: check {check} FAIL {what}")


def setup_error(what):
    print(f"check-fork-push: {what}", file=sys.stderr)
    sys.exit(2)


def git(repo, *args, text=True):
    return subprocess.run(["git", "-C", repo, *args], capture_output=True, text=text, check=True).stdout


class Pushed:
    """The objects one push publishes: blobs (with every path they appear at), the tree CI reads,
    the commit messages and the ref name."""

    def __init__(self, repo, tree_rev, objects_range, messages, ref):
        self.repo, self.messages, self.ref = repo, messages, ref
        self.tree = []  # (mode, sha, path) of the tree CI checks out
        for line in git(repo, "ls-tree", "-r", "-z", tree_rev).split("\0"):
            if line:
                meta, path = line.split("\t", 1)
                mode, kind, sha = meta.split()
                if kind == "blob":
                    self.tree.append((mode, sha, path))
        self.blobs = {}  # sha -> [paths]
        if objects_range is None:
            for _, sha, path in self.tree:
                self.blobs.setdefault(sha, []).append(path)
        else:
            out = git(repo, "rev-list", "--objects", objects_range)
            shas = {}
            for line in out.splitlines():
                sha, _, path = line.partition(" ")
                shas[sha] = path
            kinds = self.batch_check(list(shas))
            for sha, path in shas.items():
                if kinds.get(sha) == "blob":
                    self.blobs.setdefault(sha, []).append(path or sha)
        # The tip tree's blobs too: the tree-level checks read them even where the range has none.
        tree_only = {sha for _, sha, _ in self.tree} - self.blobs.keys()
        everything = self.read(list(self.blobs) + sorted(tree_only))
        self.contents = {sha: everything[sha] for sha in self.blobs}  # what is pushed
        self.by_path = {path: everything[sha] for _, sha, path in self.tree}

    def batch_check(self, shas):
        p = subprocess.run(["git", "-C", self.repo, "cat-file", "--batch-check"], input="\n".join(shas) + "\n",
                           capture_output=True, text=True, check=True)
        return {l.split()[0]: l.split()[1] for l in p.stdout.splitlines() if len(l.split()) >= 2}

    def read(self, shas):
        p = subprocess.Popen(["git", "-C", self.repo, "cat-file", "--batch"], stdin=subprocess.PIPE,
                             stdout=subprocess.PIPE)

        def feed():
            p.stdin.write("".join(s + "\n" for s in shas).encode())
            p.stdin.close()

        feeder = threading.Thread(target=feed, daemon=True)
        feeder.start()
        out = {}
        for sha in shas:
            header = p.stdout.readline().split()
            size = int(header[2])
            out[sha] = p.stdout.read(size + 1)[:size]
        p.wait()
        feeder.join()
        return out

    def path(self, sha):
        return self.blobs[sha][0]

    def tree_blobs(self):
        for mode, sha, path in self.tree:
            yield mode, path, self.by_path[path]


# ---- Decompression -----------------------------------------------------------------------------

def compression_kind(data):
    if data.startswith(b"\x1f\x8b"):
        return "gzip"
    if data.startswith(b"\x28\xb5\x2f\xfd"):
        return "zstd"
    if data.startswith(b"\xfd7zXZ\x00"):
        return "xz"
    if data.startswith(b"BZh") and data[3:4].isdigit():
        return "bzip2"
    if data.startswith((b"PK\x03\x04", b"PK\x05\x06")):
        return "zip"
    return None


def _decompressor(kind):
    if kind == "gzip":
        return zlib.decompressobj(31)
    if kind == "zstd":
        if zstd is None:
            setup_error("Python 3.14's compression.zstd is required (checks 1 and 2 decode zstd blobs)")
        return zstd.ZstdDecompressor()
    if kind == "xz":
        return lzma.LZMADecompressor()
    return bz2.BZ2Decompressor()


def _stream_members(kind, data, out, budget):
    """Decodes every member of a gzip/zstd/xz/bzip2 stream into `out` (a file), at most `budget`
    bytes. Returns (bytes written, error or None). Output up to an error is kept."""
    written, buf = 0, data
    while True:
        d = _decompressor(kind)
        try:
            if kind == "gzip":
                while not d.eof:
                    chunk = d.decompress(buf, DECODE_STEP)
                    buf = d.unconsumed_tail
                    if not chunk and not buf:
                        return written, "truncated"
                    written += len(chunk)
                    if written > budget:
                        return written, "decoded size over the cap"
                    out.write(chunk)
            else:
                chunk = d.decompress(buf, max_length=DECODE_STEP)
                while True:
                    written += len(chunk)
                    if written > budget:
                        return written, "decoded size over the cap"
                    out.write(chunk)
                    if d.eof:
                        break
                    if d.needs_input:
                        return written, "truncated"
                    chunk = d.decompress(b"", max_length=DECODE_STEP)
        except (zlib.error, lzma.LZMAError, OSError, EOFError, ValueError) as e:
            return written, f"corrupt ({type(e).__name__})"
        except Exception as e:  # compression.zstd.ZstdError and the like
            return written, f"corrupt ({type(e).__name__})"
        buf = d.unused_data
        if not buf.strip(b"\0"):
            return written, None
        if compression_kind(buf) != kind:
            return written, "trailing bytes after the last member"


def decode_layers(data, work, name, budget):
    """The decoded layers of a compressed blob, written to files under `work`: a list of
    (file, decoded size) and a list of errors. Layers are decoded again up to DECODE_DEPTH."""
    layers, errors, pending = [], [], [(data, 1)]
    while pending:
        blob, depth = pending.pop()
        kind = compression_kind(blob)
        if kind is None:
            continue
        if depth > DECODE_DEPTH:
            errors.append(f"compressed more than {DECODE_DEPTH} levels deep")
            continue
        members = []
        if kind == "zip":
            try:
                with zipfile.ZipFile(io.BytesIO(blob)) as z:
                    for info in z.infolist():
                        if info.is_dir():
                            continue
                        f = os.path.join(work, f"{name}-{len(layers) + len(members)}")
                        with z.open(info) as src, open(f, "wb") as dst:
                            n = 0
                            while chunk := src.read(DECODE_STEP):
                                n += len(chunk)
                                if n > budget:
                                    errors.append("zip: decoded size over the cap")
                                    break
                                dst.write(chunk)
                        members.append((f, n))
                        budget -= n
            except (zipfile.BadZipFile, zlib.error, NotImplementedError, RuntimeError, EOFError, OSError) as e:
                errors.append(f"zip: corrupt or unsupported ({type(e).__name__})")
        else:
            f = os.path.join(work, f"{name}-{len(layers)}")
            with open(f, "wb") as out:
                n, error = _stream_members(kind, blob, out, budget)
            members.append((f, min(n, budget)))
            budget -= min(n, budget)
            if error:
                errors.append(f"{kind}: {error}")
        for f, n in members:
            layers.append((f, n))
            if n <= NESTED_CAP:
                with open(f, "rb") as src:
                    head = src.read(8)
                if compression_kind(head) is not None:
                    pending.append((open(f, "rb").read(), depth + 1))
            elif compression_kind(open(f, "rb").read(8)) is not None:
                errors.append("a nested compressed layer over the inspection size")
    return layers, errors


def decode_pushed(pushed, work):
    """Decodes every compressed pushed blob once: {sha: [decoded files]}; decode errors fail check 1
    (the rest of such a blob cannot be seen)."""
    root = os.path.join(work, "decoded")
    os.makedirs(root, mode=0o700)
    decoded, total = {}, 0
    for sha, data in pushed.contents.items():
        if compression_kind(data) is None:
            continue
        layers, errors = decode_layers(data, root, sha, min(DECODE_CAP, DECODE_TOTAL_CAP - total))
        total += sum(n for _, n in layers)
        decoded[sha] = [f for f, _ in layers]
        for e in errors:
            fail(1, f"{pushed.path(sha)}: cannot be fully decoded ({e})")
    return decoded


# ---- Check 1: the denylist ---------------------------------------------------------------------

CF = re.compile("[" + "".join(re.escape(chr(c)) for c in range(0x110000)
                              if unicodedata.category(chr(c)) == "Cf") + "]")
# After a line break: the comment or quote leader of the next line.
LEADER = re.compile(r"[\n\r\x0b\x0c\x85  ][ \t]*(?://[/!]?|#+|--|;+|>+|%+|\*(?!/))?")
# Adjacent string literals: "a" "b", "a" + "b", "a", "b", and a backslash line continuation.
LITERAL_JOIN = re.compile(r"\"\s*[+,]?\s*\"|'\s*[+,]?\s*'|\\\n[ \t]*")
SPACE = re.compile(r"\s+")


def normalized(data):
    """The original form: ASCII case folded, whitespace runs collapsed."""
    return re.sub(rb"\s+", b" ", data.lower())


def text_forms(data):
    """The matching forms of some bytes: NFKC, format characters removed (and, separately, read as
    spaces), case folded, comment leaders dropped after line breaks, adjacent string literals
    joined, whitespace collapsed."""
    if data.isascii():
        variants = [data.decode("ascii").lower()]
    else:
        text = unicodedata.normalize("NFKC", data.decode("utf-8", "surrogateescape"))
        variants = [CF.sub("", text).casefold(), CF.sub(" ", text).casefold()]
    out = []
    for v in variants:
        v = LITERAL_JOIN.sub("", LEADER.sub("\n", v))
        out.append(SPACE.sub(" ", v))
    return out


# Characters that IGNORECASE matches to an ASCII letter but that casefolding leaves alone (or that
# only occur when a text is not casefolded): where one occurs, a regex term's prefix gate is skipped.
CASE_SPECIALS = "\u0130\u0131\u017f\u212a"


def literal_prefixes(pattern, flags):
    """A set of lowercase ASCII strings, one of which starts every match of `pattern` (as text
    case-insensitively equal to it), or None when no such set of strings of 3 or more characters
    is known. Used only to skip a regex search that cannot match."""
    from re import _constants as C, _parser

    def seq(items, prefixes):
        # Extends each prefix by `items` in order; returns (prefixes, whether every item was used).
        for op, av in items:
            if op is C.AT:
                continue
            if op is C.LITERAL and av < 128:
                prefixes = {p + chr(av).lower() for p in prefixes}
            elif op is C.IN and all(o is C.LITERAL and a < 128 for o, a in av) and len(av) <= 8:
                prefixes = {p + chr(a).lower() for p in prefixes for _, a in av}
            elif op is C.SUBPATTERN:
                prefixes, whole = seq(av[3], prefixes)
                if not whole:
                    return prefixes, False
            elif op is C.BRANCH:
                out = set()
                for branch in av[1]:
                    out |= seq(branch, prefixes)[0]
                return out, False
            elif op in (C.MAX_REPEAT, C.MIN_REPEAT) and av[0] >= 1:
                return seq(av[2], prefixes)[0], False
            else:
                return prefixes, False
            if len(prefixes) > 64:
                return prefixes, False
        return prefixes, True

    try:
        prefixes = seq(_parser.parse(pattern, flags), {""})[0]
    except Exception:  # an unexpected parse tree: search without a gate
        return None
    if not prefixes or len(prefixes) > 64 or min(map(len, prefixes)) < 3:
        return None
    return prefixes


class Term:
    def __init__(self, line):
        if line.startswith("re:"):
            self.raw = re.compile(line[3:].encode(), re.I)
            self.text = re.compile(line[3:], re.I)
            self.gate = literal_prefixes(line[3:], re.I)
        else:
            self.raw = normalized(line.encode())  # bytes.find on ASCII-lowered text = ASCII re.I
            self.text = SPACE.sub(" ", CF.sub("", unicodedata.normalize("NFKC", line)).casefold())
            self.gate = None

    def search(self, raw, forms):
        if isinstance(self.raw, bytes):
            return self.raw in raw or any(self.text in f for f in forms)
        if self.gate is None:
            return bool(self.raw.search(raw)) or any(self.text.search(f) for f in forms)
        if any(g.encode() in raw for g in self.gate) and self.raw.search(raw):
            return True
        return any(self.text.search(f) for f in forms
                   if any(c in f for c in CASE_SPECIALS) or any(g in f for g in self.gate))


def private_file(path, what):
    """The file's content, refusing a file or directory another user could read."""
    try:
        st, dst = os.stat(path), os.stat(os.path.dirname(path))
    except OSError:
        setup_error(f"{what} is missing")
    if stat.S_IMODE(st.st_mode) & 0o077 or stat.S_IMODE(dst.st_mode) & 0o077:
        setup_error(f"{what} or its directory is readable by others (want 0600 in 0700)")
    return open(path, encoding="utf-8").read()


def load_denylist(path):
    terms = []
    for line in private_file(path, "the denylist").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        terms.append(Term(line))
    if not terms:
        setup_error("the denylist is empty")
    return terms


# Worker state for check 1 (inherited through fork).
_TERMS, _CONTENTS = [], {}


def _scan_text(item):
    """Whether one check-1 item holds a term: ("blob", sha), ("file", path, offset) or ("text", bytes)."""
    if item[0] == "blob":
        chunks = [_CONTENTS[item[1]]]
    elif item[0] == "text":
        chunks = [item[1]]
    else:
        with open(item[1], "rb") as f:
            f.seek(item[2])
            chunks = [f.read(CHUNK + OVERLAP)]
    for data in chunks:
        raw, forms = normalized(data), text_forms(data)
        if any(t.search(raw, forms) for t in _TERMS):
            return True
    return False


def check1(pushed, terms, decoded, threads):
    """Denylist over every pushed blob, the decoded content of compressed blobs, the commit
    messages and the ref name."""
    global _TERMS, _CONTENTS
    items = []  # (label, item)
    for sha in pushed.contents:
        items.append((pushed.path(sha), ("blob", sha)))
        for f in decoded.get(sha, []):
            for offset in range(0, max(os.path.getsize(f), 1), CHUNK):
                items.append((pushed.path(sha) + " (decompressed)", ("file", f, offset)))
    for i, message in enumerate(pushed.messages):
        items.append(("commit message" + (f" {i + 1}" if len(pushed.messages) > 1 else ""),
                      ("text", message.encode())))
    items.append(("ref name", ("text", pushed.ref.encode())))
    _TERMS, _CONTENTS = terms, pushed.contents
    ctx = multiprocessing.get_context("fork")
    with concurrent.futures.ProcessPoolExecutor(max_workers=threads, mp_context=ctx) as pool:
        found = list(pool.map(_scan_text, [item for _, item in items], chunksize=4))
    for label in sorted({label for (label, _), hit in zip(items, found) if hit}):
        fail(1, label)
    return len(items)


# ---- Check 2: the content fingerprint ----------------------------------------------------------

def public_sources(spec, repo):
    """(git dir, [ref prefixes]) of the public reference list, keeping only prefixes of remotes
    whose URL is a public GitHub repository."""
    sources = []
    for line in open(spec):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        src, *prefixes = line.split()
        src = repo if src == "." else os.path.expanduser(src)
        if not os.path.isdir(src):
            print(f"check-fork-push: check 2 note: public source {src} is absent; its content is not exempt")
            continue
        kept = []
        for prefix in prefixes:
            remote = prefix.removeprefix("refs/remotes/").strip("/")
            url = subprocess.run(["git", "-C", src, "remote", "get-url", remote], capture_output=True,
                                 text=True).stdout.strip()
            if prefix.startswith("refs/remotes/") and PUBLIC_GIT.match(url):
                kept.append(prefix)
            else:
                print(f"check-fork-push: check 2 note: {src} {prefix} is not a public GitHub remote; not exempt")
        if kept:
            sources.append((src, kept))
    return sources


def public_blobs(sources, out):
    """Writes "<git dir>\t<sha>" for every blob in the tip trees of the public refs."""
    n = 0
    with open(out, "w") as o:
        for src, prefixes in sources:
            seen = set()
            for head in git(src, "for-each-ref", "--format=%(objectname)", *prefixes).split():
                for entry in git(src, "ls-tree", "-r", head).splitlines():
                    meta = entry.split("\t", 1)[0].split()
                    if meta[1] == "blob" and meta[2] not in seen:
                        seen.add(meta[2])
                        o.write(f"{src}\t{meta[2]}\n")
            n += len(seen)
    return n


def public_ids(sources, out):
    """Writes the id of every blob in the history of the public refs (a private file equal to one
    is public content)."""
    n = 0
    with open(out, "w") as o:
        for src, prefixes in sources:
            refs = git(src, "for-each-ref", "--format=%(refname)", *prefixes).split()
            if not refs:
                continue
            p = subprocess.run(["git", "-C", src, "rev-list", "--objects", "--no-object-names",
                                "--filter=object:type=blob", "--stdin"], input="\n".join(refs) + "\n",
                               capture_output=True, text=True, check=True)
            o.write(p.stdout)
            n += p.stdout.count("\n")
    return n


def check2(pushed, decoded, args, work):
    """Content fingerprint against the private registry (fork-push-fp)."""
    blobs = os.path.join(work, "fp-blobs.txt")
    with open(blobs, "w") as o:
        for sha, paths in pushed.blobs.items():
            for p in paths:
                o.write(f"{sha}\t{p}\n")
            for f in decoded.get(sha, []):
                o.write(f"file:{f}\t{paths[0]} (decompressed)\n")
    cmd = [args.fp_bin, "--repo", pushed.repo, "--blobs", blobs, "--private", args.registry,
           "--out", os.path.join(work, "fp-report.txt"), "--spool", os.path.join(work, "fp-spool"),
           "--threads", str(args.threads)]
    if args.public:
        sources = public_sources(args.public, pushed.repo)
        listing, ids = os.path.join(work, "fp-public.txt"), os.path.join(work, "fp-public-ids.txt")
        print(f"check-fork-push: check 2 public blobs: {public_blobs(sources, listing)} "
              f"(tip trees), {public_ids(sources, ids)} ids (history)")
        cmd += ["--public", listing, "--public-ids", ids]
    cfg = os.path.join(args.registry, "fork-push")
    for flag, name in (("--exclude", "fingerprint-scope.txt"), ("--pins", "fingerprint-pins.txt")):
        if os.path.exists(os.path.join(cfg, name)):
            private_file(os.path.join(cfg, name), f"fork-push/{name}")
            cmd += [flag, os.path.join(cfg, name)]
    if args.write_pins:
        cmd += ["--write-pins", args.write_pins]
    r = subprocess.run(cmd, capture_output=True, text=True)
    for line in r.stderr.splitlines():
        # The summary carries counts only; error lines name private paths, so only their count is shown.
        if line.startswith("fork-push-fp: ") and "error:" not in line:
            print(f"check-fork-push: check 2 {line[len('fork-push-fp: '):]}")
    if r.returncode not in (0, 1):
        fail(2, f"fork-push-fp exited {r.returncode} ({sum('error:' in l for l in r.stderr.splitlines())} error lines)")
        return
    rows = [l.split("\t") for l in open(os.path.join(work, "fp-report.txt")).read().splitlines()]
    if rows:
        reports = os.path.join(cfg, "reports")
        os.makedirs(reports, mode=0o700, exist_ok=True)
        dest = os.path.join(reports, f"{args.label}.txt")
        shutil.copyfile(os.path.join(work, "fp-report.txt"), dest)
        os.chmod(dest, 0o600)
        print(f"check-fork-push: check 2 detail (private): {dest}")
    for path in sorted({row[0] for row in rows}):
        fail(2, path)


# ---- Check 3: pointers -------------------------------------------------------------------------

def check3(pushed, pin_file):
    """Pointer report: files that name a codename, the private-test switch or the registry path and
    are not in the pinned set. Warns only (DEC-670)."""
    pinned = set()
    if os.path.exists(pin_file):
        pinned = {l.strip() for l in open(pin_file) if l.strip() and not l.startswith("#")}
    found = {path for _, path, data in pushed.tree_blobs() if POINTER.search(data)}
    for path in sorted(found - pinned):
        print(f"check-fork-push: check 3 WARN new pointer file: {path}")
    gone = pinned - found
    print(f"check-fork-push: check 3 pointer files: {len(found)} ({len(found - pinned)} new, "
          f"{len(gone)} pinned but no longer pointing)")
    return sorted(found)


# ---- Allowlist (checks 5 and 6) ----------------------------------------------------------------

class Allow:
    """`contrib/node/fork-push-allow.txt`: "<path or glob> <check> <reason>[:<arg>] [key=value ...]".
    Reasons are a closed vocabulary (REASONS); there is no free text."""

    def __init__(self, path):
        self.entries = []
        for n, line in enumerate(open(path) if os.path.exists(path) else [], 1):
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            pattern, check, reason, *extra = line.split()
            name, _, arg = reason.partition(":")
            check = int(check)
            if name not in REASONS.get(check, ()) or bool(arg) != (name in ARG_REASONS):
                setup_error(f"{path}:{n}: reason {reason!r} is not one of check {check}'s codes")
            fields = dict(e.split("=", 1) for e in extra)
            if any(c in pattern for c in "*?[") and name not in GLOB_REASONS:
                setup_error(f"{path}:{n}: {name} entries name one file, not a glob")
            self.entries.append({"pattern": pattern, "check": check, "reason": name, "arg": arg,
                                 "fields": fields, "line": n})

    def find(self, check, path):
        for e in self.entries:
            if e["check"] == check and fnmatch.fnmatchcase(path, e["pattern"]):
                return e
        return None


def named_in_ci(pushed, needle):
    return any(path.startswith(".github/") and needle.encode() in data for _, path, data in pushed.tree_blobs())


def root_workspace(pushed):
    try:
        ws = tomllib.loads(pushed.by_path["Cargo.toml"].decode()).get("workspace", {})
    except (KeyError, UnicodeDecodeError, tomllib.TOMLDecodeError):
        return [], []
    return ws.get("members", []), ws.get("exclude", [])


def nested_workspace_ok(pushed, path):
    """Guard of `nested-workspace`: the manifest's directory is not a root member (or is excluded),
    is a workspace of its own (or excluded), and CI never names it."""
    d = os.path.dirname(path)
    members, exclude = root_workspace(pushed)
    if any(fnmatch.fnmatchcase(d, m) for m in members) and d not in exclude:
        return "a root workspace member"
    manifest = pushed.by_path.get(f"{d}/Cargo.toml", b"")
    if d not in exclude and not re.search(rb"^\[workspace\]", manifest, re.M):
        return "not a workspace of its own"
    if named_in_ci(pushed, d):
        return "named by a CI file"
    return None


def allowed(pushed, allow, check, path, data, failure):
    """Whether `failure` of `path` is allowlisted; a broken guard is itself a failure."""
    e = allow.find(check, path)
    if e is None:
        return False
    reason, why = e["reason"], None
    if reason == "nested-workspace":
        why = nested_workspace_ok(pushed, path) if failure == "cargo" else "only covers Cargo sources"
    elif failure == "cargo":
        why = "only nested-workspace covers Cargo sources"
    elif reason == "local-tool":
        why = "named by a CI file" if named_in_ci(pushed, path) else None
    elif reason in ARG_REASONS and e["arg"].lower().encode() not in data.lower():
        why = f"{e['arg']} no longer occurs in it"
    if why is None and check == 5 and failure == "local" and reason not in GLOB_REASONS:
        counts = {k: len(r.findall(data)) for k, r in LOCAL_PATHS.items()}
        want = {k: int(e["fields"].get(k, 0)) for k in LOCAL_PATHS}
        if counts != want:
            why = "its local path count changed (review it again)"
    if why is None and check == 6 and e["fields"].get("blob") != hashlib.sha1(
            b"blob %d\0" % len(data) + data).hexdigest():
        why = "the file changed since it was reviewed"
    if why:
        fail(check, f"{path}: allowlist line {e['line']} ({reason}): {why}")
    return True


# ---- Check 5: local paths and machine config ---------------------------------------------------

def toml_deps(doc):
    """(table name, dependency name, spec) for every dependency-like table of a Cargo.toml."""
    def tables(d, prefix):
        for key in ("dependencies", "dev-dependencies", "build-dependencies"):
            for name, spec in (d.get(key) or {}).items():
                yield f"{prefix}{key}", name, spec
    yield from tables(doc, "")
    yield from tables(doc.get("workspace") or {}, "workspace.")
    for target, d in (doc.get("target") or {}).items():
        yield from tables(d, f"target.{target}.")
    for source, d in (doc.get("patch") or {}).items():
        for name, spec in d.items():
            yield f"patch.{source}", name, spec
    for name, spec in (doc.get("replace") or {}).items():
        yield "replace", name, spec


def read_sources(path):
    """fork-push-sources.txt: "<local url> <public url> <tag glob>" lines."""
    out = []
    for line in open(path) if os.path.exists(path) else []:
        line = line.strip()
        if line and not line.startswith("#"):
            local, public, tags = line.split()
            out.append({"local": local, "public": public, "tags": tags})
    return out


def url_map():
    """CHECK_FORK_PUSH_GIT_URL_MAP ("<url>=<path>;..."), set by the selftest only."""
    return dict(p.split("=", 1) for p in os.environ.get("CHECK_FORK_PUSH_GIT_URL_MAP", "").split(";") if p)


def ls_remote(url, *patterns):
    r = subprocess.run(["git", "ls-remote", url_map().get(url, url), *patterns], capture_output=True, text=True)
    if r.returncode != 0:
        return None
    refs = {}
    for line in r.stdout.splitlines():
        sha, ref = line.split("\t")
        refs[ref] = sha
    return refs


def check_published(pushed, sources, work, repo, tree):
    """For a tree whose root pins were rewritten to a public fork: each pinned rev is exactly a
    published tag; each upstream rev a [patch] stands in for is an ancestor of the upstream default
    branch; and the rewritten Cargo.lock resolves unchanged (cargo metadata --locked --offline, with
    the fork URL replaced by the local clone in an untracked config)."""
    try:
        doc = tomllib.loads(pushed.by_path["Cargo.toml"].decode())
    except (KeyError, UnicodeDecodeError, tomllib.TOMLDecodeError):
        return
    by_public = {s["public"]: s for s in sources}
    pins = set()  # (public url, rev)
    for table, _, spec in toml_deps(doc):
        if isinstance(spec, dict) and spec.get("git") in by_public:
            if "rev" not in spec:
                fail(5, f"Cargo.toml: [{table}] {spec['git']}: a fork source needs a rev")
                continue
            pins.add((spec["git"], spec["rev"]))
    if not pins:
        return
    for public, rev in sorted(pins):
        refs = ls_remote(public, by_public[public]["tags"])
        if refs is None:
            fail(5, f"Cargo.toml: could not list the tags of {public}")
        # A tag's commit: the peeled line of an annotated tag, or a lightweight tag itself.
        elif not any(sha == rev and (ref.endswith("^{}") or f"{ref}^{{}}" not in refs) for ref, sha in refs.items()):
            fail(5, f"Cargo.toml: {public} rev {rev[:12]} is not exactly a published {by_public[public]['tags']} tag")
    # Upstream revs that a [patch] stands in for (Cargo still loads them).
    manifests = [(p, d) for p, d in pushed.by_path.items() if os.path.basename(p) == "Cargo.toml"]
    for upstream, entries in (doc.get("patch") or {}).items():
        fork = {s.get("git") for s in entries.values() if isinstance(s, dict)} & set(by_public)
        if not fork:
            continue
        local = by_public[fork.pop()]["local"].removeprefix("file://")
        revs = set()
        for _, data in manifests:
            try:
                for _, _, spec in toml_deps(tomllib.loads(data.decode())):
                    if isinstance(spec, dict) and spec.get("git") == upstream and "rev" in spec:
                        revs.add(spec["rev"])
            except (UnicodeDecodeError, tomllib.TOMLDecodeError):
                continue
        head = (ls_remote(upstream, "HEAD") or {}).get("HEAD")
        if head is None:
            fail(5, f"Cargo.toml: could not read {upstream}'s default branch")
            continue
        for rev in sorted(revs):
            r = subprocess.run(["git", "-C", local, "merge-base", "--is-ancestor", rev, head], capture_output=True)
            if r.returncode != 0:
                fail(5, f"Cargo.toml: upstream rev {rev[:12]} is not on {upstream}'s default branch "
                        f"{head[:12]} (or {local} lacks it: fetch origin)")
    # The rewritten lock resolves as committed.
    export = os.path.join(work, "export")
    os.makedirs(os.path.join(export, ".cargo"))
    archive = subprocess.run(["git", "-C", repo, "archive", "--format=tar", tree], capture_output=True, check=True)
    with tarfile.open(fileobj=io.BytesIO(archive.stdout)) as t:
        t.extractall(export, filter="data")
    config = []
    for i, (public, rev) in enumerate(sorted(pins)):
        config += [f'[source.fork-push-public-{i}]', f'git = "{public}"', f'rev = "{rev}"',
                   f'replace-with = "fork-push-local-{i}"', f'[source.fork-push-local-{i}]',
                   f'git = "{by_public[public]["local"]}"', f'rev = "{rev}"', ""]
    open(os.path.join(export, ".cargo", "config.toml"), "w").write("\n".join(config))
    # Run from the export: cargo reads .cargo/config.toml from its working directory.
    r = subprocess.run(["cargo", "metadata", "--locked", "--offline", "--format-version", "1"], cwd=export,
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    if r.returncode != 0:
        fail(5, "Cargo.lock: the rewritten lock does not resolve unchanged "
                f"(cargo metadata --locked --offline: {r.stderr.strip().splitlines()[-1][:200] if r.stderr.strip() else r.returncode})")


def check5(pushed, allow, sources, work, tree):
    """Local paths and machine config."""
    for mode, path, data in pushed.tree_blobs():
        name = os.path.basename(path)
        if name in ("config", "config.toml") and os.path.basename(os.path.dirname(path)) == ".cargo":
            text = data.decode("utf-8", "replace")
            if re.search(r"^\s*\[(source|patch)\b|replace-with|^\s*paths\s*=", text, re.M) or "file://" in text:
                fail(5, f"{path}: source replacement, [patch] or paths override")
        if name == "Cargo.toml":
            try:
                doc = tomllib.loads(data.decode("utf-8"))
            except (UnicodeDecodeError, tomllib.TOMLDecodeError):
                fail(5, f"{path}: does not parse as TOML")
                continue
            bad = []
            for table, dep, spec in toml_deps(doc):
                if not isinstance(spec, dict):
                    continue
                if "git" in spec and not PUBLIC_GIT.match(str(spec["git"])):
                    bad.append(f"{path}: [{table}] {dep}: git source is not a public https://github.com/ URL")
                p = spec.get("path")
                if p is not None and (os.path.isabs(p) or os.path.normpath(os.path.join(os.path.dirname(path), p)).startswith("..")):
                    bad.append(f"{path}: [{table}] {dep}: path outside the repository")
            if bad and not allowed(pushed, allow, 5, path, data, "cargo"):
                for b in bad:
                    fail(5, b)
        if name == "Cargo.lock" and re.search(rb'^source = "git\+(?!https://github\.com/)', data, re.M):
            if not allowed(pushed, allow, 5, path, data, "cargo"):
                fail(5, f"{path}: a git source that is not https://github.com/")
        if not DOC.search(path) and any(r.search(data) for r in LOCAL_PATHS.values()):
            if not allowed(pushed, allow, 5, path, data, "local"):
                fail(5, f"{path}: a local machine path")
    check_published(pushed, sources, work, pushed.repo, tree)


# ---- Check 6: size and data --------------------------------------------------------------------

def check6(pushed, max_blob, allow):
    """Size and data: no blob at or above the limit, no datadir, chain snapshot or database."""
    for sha, data in pushed.contents.items():
        for path in pushed.blobs[sha]:
            if len(data) >= max_blob:
                fail(6, f"{path}: {len(data)} bytes (limit {max_blob})")
            kind = None
            if DATADIR_NAME.search(path):
                kind = "a datadir or database file name"
            elif data.startswith(b"SQLite format 3\0"):
                kind = "an SQLite database"
            elif len(data) >= 48 and data[-8:] in (LEVELDB_FOOTER, ROCKSDB_FOOTER):
                kind = "a LevelDB/RocksDB table"
            elif data[:4] in NETWORK_MAGICS and len(data) >= 88:
                # blk*.dat: magic, a 4-byte length, the block, then the next record's magic. (A P2P
                # capture has an ASCII command after the magic, which reads as a huge length.)
                size = int.from_bytes(data[4:8], "little")
                if size >= 80 and data[8 + size:12 + size] in NETWORK_MAGICS:
                    kind = "a raw block file"
            if kind and not allowed(pushed, allow, 6, path, data, "data"):
                fail(6, f"{path}: {kind}")


# ---- Check 7: secrets --------------------------------------------------------------------------

def check7(pushed, args, work):
    """Secret scan (gitleaks, pinned, with its default rules and no suppression the pushed content
    controls) of the pushed blobs, messages and ref name, against the tracked baseline of known
    public test values (rule and SHA-256 of the value)."""
    binary = args.gitleaks
    if not (binary and os.path.isfile(binary)):
        fail(7, f"gitleaks {GITLEAKS_VERSION} not found (install: check-fork-push.sh --install-gitleaks <dir>)")
        return
    if hashlib.sha256(open(binary, "rb").read()).hexdigest() != GITLEAKS_SHA256:
        fail(7, f"gitleaks at {binary} is not the pinned {GITLEAKS_VERSION} build")
        return
    # Every blob at every pushed path (gitleaks' path rules apply as on a checkout), and again
    # under a neutral name (its default config skips some paths, such as *.bin, entirely).
    root = os.path.join(work, "gitleaks-tree")
    os.makedirs(root, mode=0o700)
    names = {}

    def put(rel, data, label):
        os.makedirs(os.path.dirname(os.path.join(root, rel)), exist_ok=True)
        open(os.path.join(root, rel), "wb").write(data)
        names[rel] = label

    for sha, data in pushed.contents.items():
        for rel in pushed.blobs[sha]:
            if rel and not rel.startswith(("/", ".objects/", ".push/")) and ".." not in rel.split("/") \
                    and rel not in names:
                put(rel, data, rel)
        put(f".objects/{sha}", data, pushed.path(sha))
    for i, message in enumerate(pushed.messages):
        put(f".push/message-{i}", message.encode(), "commit message")
    put(".push/ref", (pushed.ref + "\n").encode(), "ref name")
    config, ignore = os.path.join(work, "gitleaks.toml"), os.path.join(work, "gitleaksignore")
    open(config, "w").write("[extend]\nuseDefault = true\n")
    open(ignore, "w").close()
    env = {k: v for k, v in os.environ.items() if not k.startswith("GITLEAKS")}
    report = os.path.join(work, "gitleaks.json")
    r = subprocess.run([binary, "dir", root, "--no-banner", "--log-level", "error", "--max-archive-depth", "4",
                        "--config", config, "--gitleaks-ignore-path", ignore, "--ignore-gitleaks-allow",
                        "--report-format", "json", "--report-path", report], capture_output=True, text=True, env=env)
    if r.returncode not in (0, 1) or not os.path.exists(report):
        fail(7, f"gitleaks exited {r.returncode}")
        return
    baseline = set()
    if os.path.exists(args.secrets_baseline):
        for line in open(args.secrets_baseline):
            if line.strip() and not line.startswith("#"):
                rule, digest = line.split()[:2]
                baseline.add((rule, digest))
    findings = {}  # (rule, digest) -> pushed paths
    for f in json.load(open(report)):
        rel = os.path.relpath(f["File"], root).split("!")[0]
        key = (f["RuleID"], hashlib.sha256(f["Secret"].encode()).hexdigest())
        findings.setdefault(key, set()).add(names.get(rel, rel))
    new = {k: v for k, v in findings.items() if k not in baseline}
    for (rule, digest), paths in sorted(new.items()):
        for path in sorted(paths):
            fail(7, f"{path}: {rule} (value sha256 {digest[:16]}…, not in the baseline)")
    print(f"check-fork-push: check 7 gitleaks {GITLEAKS_VERSION}: {len(findings)} distinct findings, "
          f"{len(findings) - len(new)} in the baseline")
    if args.write_secrets_baseline:
        with open(args.write_secrets_baseline, "w") as o:
            for (rule, digest), paths in sorted(findings.items(), key=lambda kv: (min(kv[1]), kv[0])):
                o.write(f"{rule} {digest} {min(paths)}\n")


def install_gitleaks(dest):
    """Downloads the pinned gitleaks release, checks its sha256, and prints the binary's path."""
    url = (f"https://github.com/gitleaks/gitleaks/releases/download/v{GITLEAKS_VERSION}/"
           f"gitleaks_{GITLEAKS_VERSION}_linux_x64.tar.gz")
    data = urllib.request.urlopen(url, timeout=120).read()
    if hashlib.sha256(data).hexdigest() != GITLEAKS_TAR_SHA256:
        setup_error("gitleaks download sha256 mismatch")
    out = os.path.join(dest, f"gitleaks-{GITLEAKS_VERSION}")
    os.makedirs(out, exist_ok=True)
    with tarfile.open(fileobj=io.BytesIO(data)) as t:
        t.extract("gitleaks", out, filter="data")
    binary = os.path.join(out, "gitleaks")
    if hashlib.sha256(open(binary, "rb").read()).hexdigest() != GITLEAKS_SHA256:
        setup_error("the extracted gitleaks binary is not the pinned build")
    print(binary)


# ---- Check 8: private tests --------------------------------------------------------------------

PRIVATE_TESTS = "contrib/node/private-tests.txt"
SKIP_GUARD = re.compile(rb"let Some\(\w+\) = private_tree\(TEST\) else \{\s*return;\s*\};")
PRIVATE_CALL = re.compile(rb"(?<!fn )\b(private_tree|embargo_dir)\s*\(")


def check8(pushed):
    """Private tests skip cleanly: nothing under .github sets DASH_EMBARGO_DIR, each listed private
    test exists and returns through node_sink::private_tree before reading anything, no unlisted
    Rust file reaches the private tree, and the tree has the CI mode that asserts it
    (check-private-tests.sh --expect-skipped)."""
    tree = pushed.by_path
    for path, data in tree.items():
        if path.startswith(".github/") and re.search(rb"DASH_EMBARGO_DIR\s*[:=]", data):
            fail(8, f"{path}: sets DASH_EMBARGO_DIR")
    if PRIVATE_TESTS not in tree:
        fail(8, f"{PRIVATE_TESTS}: missing (the list of private tests)")
        return
    listed = {}
    for line in tree[PRIVATE_TESTS].decode().splitlines():
        if line.strip() and not line.startswith("#"):
            path, test = line.split()
            listed.setdefault(path, []).append(test)
    for path, tests in listed.items():
        data = tree.get(path)
        if data is None:
            fail(8, f"{path}: listed in {PRIVATE_TESTS} but absent")
            continue
        for test in tests:
            fns = [m.end() for m in re.finditer(rb"#\[test\]\s*fn " + test.encode() + rb"\(\)\s*\{", data)]
            if not fns or not all(SKIP_GUARD.match(data, at + len(re.match(rb"\s*", data[at:]).group())) for at in fns):
                fail(8, f"{path}: {test} does not start with the private_tree skip guard")
    for path, data in tree.items():
        if path.endswith(".rs") and not path.startswith("node-sink/src/") and path not in listed \
                and PRIVATE_CALL.search(data):
            fail(8, f"{path}: reaches the private tree but is not in {PRIVATE_TESTS}")
    if b"--expect-skipped" not in tree.get("contrib/node/check-private-tests.sh", b""):
        fail(8, "contrib/node/check-private-tests.sh: no --expect-skipped mode")


# ---- Check 9: workflow triggers ----------------------------------------------------------------

def github_glob(pattern):
    """A GitHub Actions branch filter pattern as a regex."""
    out, i = "", 0
    while i < len(pattern):
        c = pattern[i]
        if pattern.startswith("**", i):
            out, i = out + ".*", i + 2
            continue
        out += {"*": "[^/]*", "?": "?", "+": "+"}.get(c) or ("[" if c == "[" else "]" if c == "]" else re.escape(c))
        i += 1
    return re.compile(out + r"\Z")


def branch_filtered(spec, branch):
    """Whether a push/create trigger spec fires for a push of `branch`."""
    if spec is None or not isinstance(spec, dict):
        return True
    if "branches" in spec:
        verdict = False
        for p in spec["branches"] or []:
            neg = str(p).startswith("!")
            if github_glob(str(p)[1:] if neg else str(p)).match(branch):
                verdict = not neg
        return verdict
    if "branches-ignore" in spec:
        return not any(github_glob(str(p)).match(branch) for p in spec["branches-ignore"] or [])
    # Only tags/tags-ignore given: branch pushes do not fire. Neither given: they do.
    return not ("tags" in spec or "tags-ignore" in spec)


def triggers(text):
    doc = yaml.safe_load(text) or {}
    on = doc.get("on", doc.get(True)) if isinstance(doc, dict) else None
    if isinstance(on, str):
        return {on: None}
    if isinstance(on, list):
        return {e: None for e in on}
    return on if isinstance(on, dict) else {}


# Events GitHub runs from the default branch whatever is pushed (deleting ci/* fires `delete`).
DEFAULT_BRANCH_EVENTS = ("schedule", "workflow_run", "delete")


def check9(pushed, args):
    """Workflow triggers: no pushed workflow but node-gate.yml fires on the pushed branch; the
    fork's default branch has no active workflow on an event it runs whatever is pushed."""
    branch = pushed.ref.removeprefix("refs/heads/")
    for _, path, data in pushed.tree_blobs():
        if not re.match(r"\.github/workflows/[^/]+\.ya?ml$", path) or os.path.basename(path) in ("node-gate.yml",):
            continue
        try:
            on = triggers(data)
        except yaml.YAMLError:
            fail(9, f"{path}: does not parse as YAML")
            continue
        for event in ("push", "create"):
            if event in on and (event == "create" or branch_filtered(on[event], branch)):
                fail(9, f"{path}: fires on {event} of {branch}")
    if args.fork_fixture:
        fork = json.load(open(args.fork_fixture))
    else:
        fork = fork_state(args.fork)
        if isinstance(fork, str):
            fail(9, f"{args.fork}: {fork}")
            return
    checked = []
    for path, wf in sorted(fork["workflows"].items()):
        try:
            on = triggers(wf["content"])
        except yaml.YAMLError:
            fail(9, f"{args.fork}@{fork['default_branch']}:{path}: does not parse as YAML")
            continue
        bad = sorted(e for e in DEFAULT_BRANCH_EVENTS if e in on)
        checked.append(f"{os.path.basename(path)}={','.join(bad) or 'none'}/{wf['state']}")
        if bad and wf["state"] == "active":
            fail(9, f"{args.fork}@{fork['default_branch']}:{path}: active {' and '.join(bad)} trigger")
    print(f"check-fork-push: check 9 fork {args.fork} default branch {fork['default_branch']}: {' '.join(checked) or 'no workflows'}")


def gh_json(path):
    try:
        r = subprocess.run(["gh", "api", path], capture_output=True, text=True)
    except FileNotFoundError:
        return None
    if r.returncode != 0:
        return None
    try:
        return json.loads(r.stdout)
    except json.JSONDecodeError:
        return None


def fork_state(fork):
    """The fork's default branch and its workflows (content and Actions state), read-only; an
    error string when any call fails or the answers disagree."""
    repo = gh_json(f"repos/{fork}")
    if not isinstance(repo, dict) or "default_branch" not in repo:
        return "could not read the repository (gh api)"
    branch = repo["default_branch"]
    listing = gh_json(f"repos/{fork}/contents/.github/workflows?ref={branch}")
    actions = gh_json(f"repos/{fork}/actions/workflows?per_page=100")
    if not isinstance(listing, list) or not isinstance(actions, dict):
        return "could not list the workflows (gh api)"
    states = {w["path"]: w["state"] for w in actions.get("workflows", [])}
    workflows = {}
    for entry in listing:
        if not re.search(r"\.ya?ml$", entry["name"]):
            continue
        blob = gh_json(f"repos/{fork}/contents/{entry['path']}?ref={branch}")
        if not isinstance(blob, dict) or not blob.get("content"):
            return f"could not read {entry['path']} (gh api)"
        workflows[entry["path"]] = {"content": base64.b64decode(blob["content"]).decode(),
                                    "state": states.get(entry["path"], "active")}
    missing = sorted(p for p, s in states.items() if s == "active" and p.startswith(".github/") and p not in workflows)
    if missing:
        return f"active workflows absent from the contents listing: {', '.join(missing)}"
    return {"default_branch": branch, "workflows": workflows}


# ---- Snapshot rewrite --------------------------------------------------------------------------

WORKFLOW = ".github/workflows/node-gate.yml"


def rewrite(args):
    """Prints the tree of `--tree` with the local sources of every Cargo.toml and Cargo.lock (the
    root's and every nested workspace's) replaced by their public URLs (fork-push-sources.txt), revs
    unchanged, and, with `--workflow`, `.github/workflows/` holding only that file as node-gate.yml
    (the fork runs no inherited workflow). A local source left in any manifest or lock fails. The
    rewritten paths go to stderr. The worktree and the index are not touched."""
    sources = read_sources(args.sources)
    tree = git(args.repo, "rev-parse", "--verify", f"{args.tree}^{{tree}}").strip()
    paths = [p for p in git(args.repo, "ls-tree", "-r", "-z", "--name-only", tree).split("\0")
             if os.path.basename(p) in ("Cargo.toml", "Cargo.lock")]
    batch = subprocess.run(["git", "-C", args.repo, "cat-file", "--batch"],
                           input="".join(f"{tree}:{p}\n" for p in paths).encode(), capture_output=True, check=True).stdout
    original, changed, at = {}, {}, 0
    for path in paths:
        header_end = batch.index(b"\n", at)
        size = int(batch[at:header_end].split()[2])
        original[path] = batch[header_end + 1:header_end + 1 + size].decode()
        at = header_end + 2 + size
    for path, data in original.items():
        new = data
        for s in sources:
            if path.endswith("Cargo.toml"):
                new = new.replace(f'"{s["local"]}"', f'"{s["public"]}"')
            else:
                # `source = "git+<url>?..."`, and `(git+<url>?...)` in older locks' dependency lists.
                new = new.replace(f'git+{s["local"]}?', f'git+{s["public"]}?')
        if any(s["local"] in new for s in sources):
            setup_error(f"{path}: a local source is left after the rewrite (not in the form it rewrites)")
        if new == data:
            continue
        if path.endswith("Cargo.toml"):
            # Only git source values may differ: map the public URLs back and compare the parsed files.
            after = json.dumps(tomllib.loads(new), default=str)
            for s in sources:
                after = after.replace(json.dumps(s["public"]), json.dumps(s["local"]))
            if json.loads(after) != json.loads(json.dumps(tomllib.loads(data), default=str)):
                setup_error(f"the source rewrite changed more of {path} than its git URLs")
        changed[path] = new
    if not changed and not args.workflow:
        print(tree)
        return
    scratch = tempfile.mkdtemp(prefix="check-fork-push-index.")
    env = {**os.environ, "GIT_INDEX_FILE": os.path.join(scratch, "index")}
    try:
        subprocess.run(["git", "-C", args.repo, "read-tree", tree], env=env, check=True)
        if args.workflow:
            old = subprocess.run(["git", "-C", args.repo, "ls-files", "-z", "--", ".github/workflows"], env=env,
                                 capture_output=True, text=True, check=True).stdout.split("\0")
            subprocess.run(["git", "-C", args.repo, "update-index", "--force-remove", "--", *[p for p in old if p]],
                           env=env, check=True)
            changed[WORKFLOW] = open(args.workflow).read()
        for path, text in changed.items():
            blob = subprocess.run(["git", "-C", args.repo, "hash-object", "-w", "--stdin"], input=text.encode(),
                                  capture_output=True, check=True).stdout.decode().strip()
            subprocess.run(["git", "-C", args.repo, "update-index", "--add", "--cacheinfo", f"100644,{blob},{path}"],
                           env=env, check=True)
            what = "the only workflow" if path == WORKFLOW else "sources to their public URLs"
            print(f"check-fork-push: rewrote {path} ({what})", file=sys.stderr)
        print(subprocess.run(["git", "-C", args.repo, "write-tree"], env=env, capture_output=True, text=True,
                             check=True).stdout.strip())
    finally:
        shutil.rmtree(scratch)


# ---- Main --------------------------------------------------------------------------------------

def receipt(args, pushed):
    """The private record of a passing snapshot check, which the push step re-verifies."""
    denylist = os.path.join(args.registry, "fork-push", "denylist.txt")
    record = {
        "tip": args.tip, "commit": args.commit, "tree": git(args.repo, "rev-parse", f"{args.commit}^{{tree}}").strip(),
        "ref": pushed.ref, "rewritten": args.rewritten.split(",") if args.rewritten else [],
        "denylist_sha256": hashlib.sha256(open(denylist, "rb").read()).hexdigest(),
        "checker": git(args.repo, "hash-object", os.path.abspath(__file__)).strip(),
        "fp_sha256": hashlib.sha256(open(args.fp_bin, "rb").read()).hexdigest(),
        "label": args.label, "time": int(time.time()), "checks": getattr(args, "checks_run", list(range(1, 10))),
    }
    os.makedirs(os.path.dirname(args.receipt), mode=0o700, exist_ok=True)
    fd = os.open(args.receipt, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        json.dump(record, f, indent=1)


def check_main(args):
    terms = load_denylist(os.path.join(args.registry, "fork-push", "denylist.txt"))
    messages = [open(f).read() for f in args.message_file]
    if args.messages_range:
        messages += [m for m in git(args.repo, "log", "--format=%B%x00", args.messages_range).split("\0") if m.strip()]
    pushed = Pushed(args.repo, args.tree, args.objects, messages, args.ref)
    print(f"check-fork-push: {len(pushed.blobs)} blobs, {len(pushed.tree)} tree entries, "
          f"{len(messages)} commit messages, ref {args.ref}")
    allow = Allow(args.allow)
    decoded = decode_pushed(pushed, args.work)
    print(f"check-fork-push: {len(decoded)} compressed blobs decoded "
          f"({sum(os.path.getsize(f) for fs in decoded.values() for f in fs)} bytes)")
    only = {int(c) for c in args.only.split(",")} if args.only else set(range(1, 10))
    if only != set(range(1, 10)):
        print(f"check-fork-push: running only checks {', '.join(map(str, sorted(only)))} (--only)")
    if 1 in only:
        n = check1(pushed, terms, decoded, args.threads)
        print(f"check-fork-push: check 1 scanned {n} texts against {len(terms)} denylist lines")
    if 2 in only:
        check2(pushed, decoded, args, args.work)
    if 3 in only:
        pointers = check3(pushed, args.pointers)
        if args.write_pointers:
            open(args.write_pointers, "w").write("".join(p + "\n" for p in pointers))
    # Check 4 (messages and ref names) runs inside check 1.
    if 5 in only:
        check5(pushed, allow, read_sources(args.sources), args.work, args.tree)
    if 6 in only:
        check6(pushed, args.max_blob, allow)
    if 7 in only:
        check7(pushed, args, args.work)
    if 8 in only:
        check8(pushed)
    if 9 in only:
        check9(pushed, args)
    args.checks_run = sorted(only)
    by_check = sorted({c for c, _ in FAILED})
    print(f"check-fork-push: {'FAIL' if FAILED else 'PASS'}"
          + (f" ({len(FAILED)} hits; checks {', '.join(map(str, by_check))})" if FAILED else ""))
    if not FAILED and args.receipt:
        receipt(args, pushed)
    sys.exit(1 if FAILED else 0)


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("check")
    c.add_argument("--repo", required=True)
    c.add_argument("--tree", required=True, help="the revision whose tree CI checks out")
    c.add_argument("--objects", help="a rev-list range of pushed objects (range mode); default: the tree")
    c.add_argument("--message-file", action="append", default=[])
    c.add_argument("--messages-range")
    c.add_argument("--ref", required=True)
    c.add_argument("--registry", required=True)
    c.add_argument("--work", required=True)
    c.add_argument("--label", required=True)
    c.add_argument("--fp-bin", required=True)
    c.add_argument("--gitleaks")
    c.add_argument("--public")
    c.add_argument("--pointers", required=True)
    c.add_argument("--allow", required=True)
    c.add_argument("--sources", required=True)
    c.add_argument("--secrets-baseline", required=True)
    c.add_argument("--fork", default="PastaPastaPasta/rust-dashcore")
    c.add_argument("--fork-fixture")
    c.add_argument("--max-blob", type=int, default=50_000_000)
    c.add_argument("--threads", type=int, default=8)
    c.add_argument("--write-pins")
    c.add_argument("--write-secrets-baseline")
    c.add_argument("--write-pointers")
    c.add_argument("--receipt", help="on PASS, write the private receipt here")
    c.add_argument("--tip")
    c.add_argument("--commit")
    c.add_argument("--rewritten", default="")
    c.add_argument("--only", help="run only these checks, e.g. 1,7 (the receipt records which ran)")
    r = sub.add_parser("rewrite")
    r.add_argument("--repo", required=True)
    r.add_argument("--tree", required=True)
    r.add_argument("--sources", required=True)
    r.add_argument("--workflow", help="the snapshot's only workflow, .github/workflows/node-gate.yml")
    g = sub.add_parser("install-gitleaks")
    g.add_argument("dir")
    args = ap.parse_args()
    if args.cmd == "check":
        check_main(args)
    elif args.cmd == "rewrite":
        rewrite(args)
    else:
        install_gitleaks(args.dir)


if __name__ == "__main__":
    main()
