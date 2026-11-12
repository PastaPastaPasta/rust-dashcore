#!/usr/bin/env python3
# EMBARGO-1
"""A shard run's inputs and its fold (BL-841b; run.sh --shard). Counts only, never names or bytes.

  shards.py stage RUN J SEED DIR...   every input of DIR... into one of RUN/shards/0..J-1
  shards.py fold RUN CORPUS           RUN/new into CORPUS, once; then RUN/shards goes

`stage` gives each regular file of the directories (the working corpus, the private seeds, the public
seeds and the fixed regression vectors, in run.sh's order) to exactly one shard, `assign(SEED, name)`:
the sha256 of the run id and the name, so the split rotates from run to run and any run's split can be
made again from its id. A name that two directories share is one input when the files are equal; a
later one with other bytes is staged as `<name>~<i>`, i its directory's position, so every input still
runs once. Each is reflinked (a copy where the filesystem has none): libFuzzer never deletes an input
it loaded, and the sources stay as they are. A file that is gone by the time it is read (a slice of the
leg's other pin reduced it) is passed over. RUN/shards.json records J, the seed, the inputs and each
shard's files and bytes.

`fold` (run.sh's end and its EXIT trap) links each file of RUN/new (libFuzzer's new inputs, named by
their sha1) into CORPUS and removes it from new/; a name already in CORPUS is the same input, counted
as a duplicate and left in new/. It runs under the leg's shared run lock, which a minimisation takes
exclusively. It counts `fold_redundant` too: the new inputs whose features (the workers'
-features_dir files, BL-841b §2.2) are all among the features of the loaded inputs and of the new
inputs before them in name order; v1 keeps them (OD-L3-161 Q4). The counts go into shards.json once;
a second fold finds them and does nothing. RUN/shards is removed after it.
"""
import fcntl, filecmp, hashlib, json, os, shutil, struct, sys

FICLONE = 0x40049409
FEATURE_SET_SIZE = 1 << 21  # libFuzzer's kFeatureSetSize: feature ids fold modulo it
RECORD = "shards.json"


def assign(seed, name, jobs):
    """The shard of input `name` in the run whose id is `seed`."""
    digest = hashlib.sha256(seed.encode() + b"\0" + os.fsencode(name)).digest()
    return int.from_bytes(digest[:8], "big") % jobs


def reflink(src, dst):
    """`dst`, a new file with `src`'s bytes (a reflink where it can be); its size."""
    with open(src, "rb") as a, open(dst, "xb") as b:
        try:
            fcntl.ioctl(b.fileno(), FICLONE, a.fileno())
        except OSError:
            shutil.copyfileobj(a, b)
        return os.fstat(a.fileno()).st_size


def write_record(run, record):
    path = os.path.join(run, RECORD)
    fd = os.open(path + ".new", os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        json.dump(record, f, sort_keys=True)
    os.replace(path + ".new", path)


def stage(run, jobs, seed, dirs):
    shards = [os.path.join(run, "shards", str(k)) for k in range(jobs)]
    for d in shards:
        os.mkdir(d, 0o700)
    counts = [{"files": 0, "bytes": 0} for _ in shards]
    first = {}  # name -> where it was staged
    same = 0
    for i, src in enumerate(dirs):
        with os.scandir(src) as it:
            entries = sorted((e for e in it if e.is_file(follow_symlinks=False)), key=lambda e: e.name)
        for e in entries:
            name = e.name
            if name in first:
                try:
                    if filecmp.cmp(first[name], e.path, shallow=False):
                        same += 1
                        continue
                except FileNotFoundError:
                    continue
                name = f"{e.name}~{i}"
            k = assign(seed, name, jobs)
            dst = os.path.join(shards[k], name)
            try:
                size = reflink(e.path, dst)
            except FileNotFoundError:
                continue
            first.setdefault(e.name, dst)
            counts[k]["files"] += 1
            counts[k]["bytes"] += size
    write_record(run, {"jobs": jobs, "seed": seed, "inputs": sum(c["files"] for c in counts),
                       "same_name_duplicates": same, "shards": counts})


def features(path):
    """The feature ids of one -features_dir file (uint32s), folded as libFuzzer folds them; None
    for a file that is not whole."""
    try:
        data = open(path, "rb").read()
    except OSError:
        return None
    if len(data) % 4:
        return None
    return {f % FEATURE_SET_SIZE for f in struct.unpack(f"<{len(data) // 4}I", data)}


def redundant(run, new_names):
    """How many of `new_names` add no feature over the loaded inputs' and the earlier new ones'."""
    by_name, union = {}, set()
    root = os.path.join(run, "features")
    for dirpath, _, files in os.walk(root):
        for f in files:
            got = features(os.path.join(dirpath, f))
            if got is None:
                continue
            if f in new_names:
                by_name.setdefault(f, set()).update(got)
            else:
                union |= got
    count = 0
    for name in sorted(new_names):
        got = by_name.get(name, set())
        if got <= union:
            count += 1
        union |= got
    return count


def fold(run, corpus):
    path = os.path.join(run, RECORD)
    try:
        record = json.load(open(path))
    except (OSError, ValueError):
        return 1  # nothing was staged
    if "folded" not in record:
        new = os.path.join(run, "new")
        try:
            names = sorted(e.name for e in os.scandir(new) if e.is_file(follow_symlinks=False))
        except OSError:
            names = []
        record["fold_redundant"] = redundant(run, set(names))
        folded = duplicates = 0
        for name in names:
            src = os.path.join(new, name)
            try:
                os.link(src, os.path.join(corpus, name))
            except FileExistsError:
                duplicates += 1
                continue
            except FileNotFoundError:
                continue
            os.unlink(src)
            folded += 1
        record.update(folded=folded, duplicates=duplicates, left=len(names) - folded)
        write_record(run, record)
    shutil.rmtree(os.path.join(run, "shards"), ignore_errors=True)
    return 0


def main(argv):
    if len(argv) >= 4 and argv[0] == "stage" and argv[2].isdigit() and int(argv[2]) >= 1:
        stage(argv[1], int(argv[2]), argv[3], argv[4:])
        return 0
    if len(argv) == 3 and argv[0] == "fold":
        return fold(argv[1], argv[2])
    print("usage: shards.py stage RUN J SEED DIR... | fold RUN CORPUS", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
