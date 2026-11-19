#!/usr/bin/env python3
"""A chain target's tree and its encoding (O2b-4 §3a.3 (3), D55): the `ENCODING` marker.

    python3 -I node-fuzz/tree_encoding.py check|adopt <tdir> <encoding>

A chain target's private tree (`$DASH_EMBARGO_DIR/fuzz/<target>/`) holds input bytes that mean
something only under one encoding, which its `ENCODING` file names. run.sh runs `adopt` before it
creates, copies or builds anything:
- a marker naming another encoding fails, `encoding-mismatch`;
- with no marker, a `chain-v1` tree (the legacy `diff_chain`'s, v1 by history) is marked once;
  a tree of any other encoding is marked only when its `corpus`, `seeds`, `regress` and
  `public-seeds` are all empty or absent, and is refused otherwise: its inputs' encoding is
  unknown (OD-L3-95 (2)). A tree a smoke left before 4e2 (its `public-seeds` filled, no marker)
  is refused so, until the manager's one-off clears it (OD-L3-96 Q6).
`check` judges the same and writes nothing. Exit 0: the tree is (or, for `check`, may be marked)
the encoding's; 3: `encoding-mismatch`; 4: unmarked and not empty; 2: anything else.
"""

import os
import re
import stat
import sys

MARKER = "ENCODING"
INPUT_DIRS = ("corpus", "seeds", "regress", "public-seeds")
LEGACY = "chain-v1"
ENCODING_NAME = re.compile(r"chain-v[1-9][0-9]*")
# The longest marker read: anything longer is not one.
MARKER_MAX = 64


class Refused(Exception):
    def __init__(self, status, message):
        super().__init__(message)
        self.status = status


def read_marker(tdir):
    """The marker's encoding, None when there is none; Refused when it is not a plain file holding
    one encoding's name and a newline."""
    path = os.path.join(tdir, MARKER)
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    except FileNotFoundError:
        return None
    except OSError as e:
        raise Refused(3, f"encoding-mismatch: {MARKER} is not a readable file ({type(e).__name__})") from e
    with os.fdopen(fd, "rb") as f:
        if not stat.S_ISREG(os.fstat(f.fileno()).st_mode):
            raise Refused(3, f"encoding-mismatch: {MARKER} is not a plain file")
        data = f.read(MARKER_MAX + 1)
    # Exactly one encoding's name and a newline, as `mark` writes it.
    name = data[:-1].decode("ascii", "replace")
    if not (data.endswith(b"\n") and len(data) <= MARKER_MAX and ENCODING_NAME.fullmatch(name)):
        raise Refused(3, f"encoding-mismatch: {MARKER} is not an encoding's name and a newline")
    return name


def holds_inputs(path):
    """Whether `path` holds anything but directories: a file, a link or anything else below it,
    or `path` itself not a directory. Absent, it holds nothing."""
    if not os.path.lexists(path):
        return False
    if os.path.islink(path) or not os.path.isdir(path):
        return True

    def unreadable(e):
        raise e

    for top, dirs, files in os.walk(path, onerror=unreadable):
        if files or any(os.path.islink(os.path.join(top, d)) for d in dirs):
            return True
    return False


def judge(tdir, encoding):
    """None when the tree is `encoding`'s, else whether it may be marked as it ("mark"); Refused
    otherwise."""
    if not ENCODING_NAME.fullmatch(encoding):
        raise Refused(2, f"not an encoding: {encoding!r}")
    marked = read_marker(tdir)
    if marked is not None:
        if marked != encoding:
            raise Refused(3, f"encoding-mismatch: the tree is {marked!r}, the target {encoding}")
        return None
    if encoding == LEGACY:
        return "mark"
    try:
        full = [d for d in INPUT_DIRS if holds_inputs(os.path.join(tdir, d))]
    except OSError as e:
        raise Refused(2, f"the tree cannot be read ({type(e).__name__})") from e
    if full:
        raise Refused(4, f"the tree has no {MARKER} and its {', '.join(full)} hold inputs of no known encoding: "
                         f"refused for {encoding}")
    return "mark"


def mark(tdir, encoding):
    """Writes the marker, once: the tree directory (0700) when absent, then `ENCODING` (0600)."""
    os.makedirs(tdir, mode=0o700, exist_ok=True)
    fd = os.open(os.path.join(tdir, MARKER), os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w") as f:
        f.write(encoding + "\n")


def main(argv):
    if len(argv) != 3 or argv[0] not in ("check", "adopt"):
        print("usage: tree_encoding.py check|adopt <tdir> <encoding>", file=sys.stderr)
        return 2
    action, tdir, encoding = argv
    try:
        if judge(tdir, encoding) == "mark" and action == "adopt":
            mark(tdir, encoding)
    except Refused as e:
        print(f"tree_encoding: {e}", file=sys.stderr)
        return e.status
    except OSError as e:
        print(f"tree_encoding: the marker could not be written ({type(e).__name__})", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
