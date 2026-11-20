#!/usr/bin/env python3
"""Raw diff inventory between two Dash Core release pins (node-program O17).

Lists every pull request merged between two pins (the first-parent commits of
the newer pin), the files each one touched and the area each file belongs to,
and splits the reviewable PRs into auditor packets. A PIN-DELTA audit uses it
to prove that every delta in the diff-stat was looked at.

    python3 -I contrib/node/pin-delta/inventory.py \\
        --core ~/workspace/dash --old v23.1.8 --new v24.0.0-rc.3 --out /work/scratch/<slug>/pin-delta
    # after the audit, check that the report and packets cite every reviewable PR:
    python3 -I contrib/node/pin-delta/inventory.py ... \\
        --report designs/O17-pin-delta.md designs/O17-pin-delta/*.md

Outputs (all in --out):
    prs.tsv       one row per first-parent commit (or per sub-PR of a backport
                  merge): side (new, or old when the old pin is not an ancestor of
                  the new one), PR, sha, tier, in_old, primary area, areas, +/- in
                  reviewable files, reviewable files that still differ between the
                  pins, via (the backport merge's PR, or -), title
    files.tsv     one row per file that differs between the pins: path, area,
                  tier, status, +/-, the PRs (or, failing that, commits) that
                  touched it, and how many old-pin release-branch commits touched it
    commits.tsv   every non-merge commit inside each first-parent commit, with its PR
    packets/<packet>.prs.txt
                  the reviewable PRs of one auditor packet, with per-file +/-, the
                  functional tests each PR touched and the commits inside each merge
    packets/<packet>.files.txt
                  the reviewable files the packet owns for its file sweep
    coverage.tsv  how often the --report files cite each PR (zero = not accounted for)
    summary.md    counts per area, packet and tier, and the coverage gaps

Backport merges: a first-parent commit that carries cherry-picked develop merges
(non-merge commits titled "Merge #N: ...", as `git cherry-pick -m1 -x` leaves them) is
split into one row per such commit, so each upstream PR gets its own tier, packet and
coverage check. A commit inside it with no PR title (a direct commit) is a row labelled
by its sha. The umbrella PR is not a row of its own (prs.tsv names it in the `via`
column); what the merge itself changed on top of its parents (`git show --remerge-diff`: a
conflict resolution, an edit, even in a file a sub-PR touched) is a residual row under the
umbrella PR, which a report must cite; so is each merge inside it (a release branch merged
back), under its own "Merge #N" number when it has one. Only a two-parent merge is split, and
not one that holds an octopus merge. `--no-split` keeps one row per first-parent commit.

Tiers: "review" (touches runtime code), "skip" (tests, GUI, docs, build and
vendored non-source files only) and "no-net-delta" (touched runtime files that
no longer differ between the pins).

in_old: when the old pin is a release branch merged back into the new pin's
line, many PRs in the range were already backported to it, so a PR diff is not
proof of a delta. Signals, joined with "+":
    yes            every commit's patch-id appears among the commits that reached
                   the old pin outside the range (the commits were cherry-picked);
    tree           the PR's diff reverse-applies to the old pin's tree, so its
                   post-image is already there;
    ref            the PR number appears in those commits' messages (a hint only:
                   "not on this branch" is written the same way);
    partial:m/n    m of the PR's n commits match by patch-id.
None of them proves "no delta": a later merge can drop part of a backport.
Always verify against the old pin's source.

Exit status: 0 clean, 1 coverage gaps or unattributed runtime files, 2 git or
usage errors.
"""

import argparse
import collections
import os
import re
import subprocess
import sys

# First match wins. Tier "skip" marks paths with no runtime behaviour on a
# node (tests, GUI, docs, build, vendored non-source files); "review" is
# everything a node, masternode, RPC client or peer can observe. Vendored
# library *sources* are reviewed: a BLS or secp256k1 change can be consensus.
AREA_RULES = [
    (r"^src/dashbls/(src|include)/(?!.*test)", "llmq-sig", "review"),
    (r"^src/secp256k1/(src|include)/(?!.*(test|bench))", "primitives", "review"),
    (r"^src/univalue/(lib|include)/", "rpc", "review"),
    (r"^src/gsl/", "util", "review"),
    (r"^src/(secp256k1|leveldb|crc32c|minisketch|univalue|dashbls|immer|gsl|crypto/ctaes)/", "vendored", "skip"),
    (r"^(depends|contrib|share|ci|\.github|build-aux|build_msvc|doc)/", "build-doc", "skip"),
    (r"^\.[^/]+$", "build-doc", "skip"),
    (r"^(test|src/test|src/wallet/test|src/bench)/", "test", "skip"),
    (r"^src/qt/", "qt", "skip"),
    (r"(^|/)(Makefile[^/]*|configure\.ac|\.clang-[a-z]+|\.bear-tidy-config|[^/]*\.md|COPYING|INSTALL)$", "build-doc", "skip"),
    (r"^src/(validation|pow|chainparams|chainparamsbase|versionbits|deploymentstatus|deploymentinfo|chain|coins|undo|txdb|merkleblock)\.", "consensus", "review"),
    (r"^src/(consensus|primitives|script|kernel)/", "consensus", "review"),
    (r"^src/(serialize|streams|hash|uint256|arith_uint256|pubkey|key|bech32|base58|key_io|outputtype|core_read|core_write|core_io|psbt)\.", "primitives", "review"),
    (r"^src/evo/", "evo", "review"),
    (r"^src/(llmq/(signing|signing_shares|ehf_signals|net_signing)|instantsend/|chainlock/|bls/|active/(chainlock|instantsend|signing|notificationinterface))", "llmq-sig", "review"),
    (r"^src/(llmq|active)/", "llmq-dkg", "review"),
    (r"^src/(masternode|governance|coinjoin)/", "mn-overlay", "review"),
    (r"^src/(spork|messagesigner|dsnotificationinterface|flat-database|msg_result|netfulfilledman)\.", "mn-overlay", "review"),
    (r"^src/(net|net_processing|net_permissions|net_types|protocol|addrman|addrman_impl|addrdb|banman|netaddress|netbase|netgroup|txrequest|txorphanage|blockencodings|i2p|torcontrol|mapport|limitedmap|timedata|version)\.", "p2p", "review"),
    (r"^src/(policy|node/mempool|node/miner)", "mempool-mining", "review"),
    (r"^src/(txmempool|validationinterface)\.", "mempool-mining", "review"),
    (r"^src/(rpc|zmq|stats)/", "rpc", "review"),
    (r"^src/(httprpc|httpserver|rest|bitcoin-cli)\.", "rpc", "review"),
    (r"^src/(wallet|interfaces)/", "wallet", "review"),
    (r"^src/(walletinitinterface|dummywallet|external_signer)\.", "wallet", "review"),
    (r"^src/index/", "index", "review"),
    (r"^src/(addressindex|spentindex|timestampindex|blockfilter)\.", "index", "review"),
    (r"^src/(init|node|common)/", "init-node", "review"),
    (r"^src/(init|bitcoind|shutdown|warnings|noui|clientversion|bitcoin-[a-z]+)\.", "init-node", "review"),
    (r"^src/", "util", "review"),
]
AREA_RE = [(re.compile(p), area, tier) for p, area, tier in AREA_RULES]

# Auditor packets: a PR goes to the packet of its primary area (the area with
# the most changed reviewable lines); a file goes to the packet of its area.
PACKETS = {
    "consensus": ("consensus", "primitives"),
    "util": ("util", "other"),
    "mempool-index": ("mempool-mining", "index"),
    "evo": ("evo",),
    "llmq-dkg": ("llmq-dkg",),
    "llmq-sig": ("llmq-sig",),
    "p2p-init": ("p2p", "init-node"),
    "mn-overlay": ("mn-overlay",),
    "rpc": ("rpc",),
    "wallet": ("wallet",),
}
PACKET_OF = {area: packet for packet, areas in PACKETS.items() for area in areas}

# A first-parent commit's own PR: "Merge #N: ..." or a trailing "(#N)".
PR_RE = re.compile(r"^Merge (?:dashpay/dash)?#(\d+)\b|\(#(\d+)\)\s*$")
# A Dash PR cited anywhere: "#N", "dash#N", "dashpay/dash#N"; not "bitcoin#N"
# or "bitcoin-core/gui#N".
DASH_REF = re.compile(r"(?:^|(?<=[^\w/#])|(?<=dash))#(\d{3,5})\b", re.I | re.M)
# A cherry-picked develop merge: a non-merge commit titled "Merge #N: ...".
SUB_PR_RE = re.compile(r"^Merge (?:dashpay/dash)?#(\d+)\b")
CHERRY_RE = re.compile(r"^\((?:partially )?cherry picked from commit ([0-9a-f]{7,40})\)\s*$", re.M)
# Diff output must not depend on the user's git config: prefixes (`git apply` in reverse_applies reads
# them back at -p1), the algorithm (it moves the add/del counts), hunk fusing and the context size
# (`git apply --check -R` refuses a zero-context patch).
DIFF_OPTS = ["--no-color", "--no-ext-diff", "--no-textconv", "--no-renames", "--src-prefix=a/", "--dst-prefix=b/",
             "--diff-algorithm=myers", "--inter-hunk-context=0"]
PATCH_OPTS = [*DIFF_OPTS, "-U3"]  # for the commands that print patches (-U would add one to --numstat)


# Config the results must not depend on: gpg output in log/show (it lands on stdout), the conflict style of
# the automatic merge behind --remerge-diff (diff3 adds the base to every conflict), and the abbreviation
# length of %h. (Diff options are pinned in DIFF_OPTS; --no-show-signature is LOG_OPTS.)
GIT_PINS = ["-c", "log.showSignature=false", "-c", "merge.conflictStyle=merge", "-c", "core.abbrev=auto"]
LOG_OPTS = ["--no-show-signature"]


def git(core, *args, env=None, data=None):
    return subprocess.run(["git", "-C", core, *GIT_PINS, *args], check=True, capture_output=True, env=env,
                          input=data).stdout


def git_text(core, *args):
    return git(core, *args).decode("utf-8", "replace")


def patch_ids(core, *revs):
    """{patch-id: sha} for the non-merge commits in revs (git rev-list syntax)."""
    log = git(core, "log", *LOG_OPTS, "-p", "--no-merges", *PATCH_OPTS, "--format=commit %H", *revs)
    out = git(core, "patch-id", "--stable", data=log).decode()
    return {pid: sha for pid, sha in (line.split() for line in out.splitlines())}


def classify(path):
    for rx, area, tier in AREA_RE:
        if rx.search(path):
            return area, tier
    return "other", "review"


def parse_numstat(numstat_text, status_text):
    status = {}
    for line in status_text.splitlines():
        st, path = line.split("\t", 1)
        status[path] = st[0]
    out = {}
    for line in numstat_text.splitlines():
        add, dele, path = line.split("\t", 2)
        out[path] = (int(add) if add != "-" else 0, int(dele) if dele != "-" else 0, status.get(path, "M"))
    return out


def numstat(core, a, b):
    """{path: (added, deleted, status)} for a..b. Renames are split into a
    delete and an add so both paths are attributed to the commit that moved them."""
    return parse_numstat(git_text(core, "diff", *DIFF_OPTS, "--numstat", a, b),
                         git_text(core, "diff", *DIFF_OPTS, "--name-status", a, b))


def merge_own_numstat(core, merge):
    """numstat of what the merge commit itself changed: its diff against git's automatic merge of its
    parents (`git show --remerge-diff`, git >= 2.36), i.e. conflict resolutions and edits made in the
    merge. Empty for a merge that is just the sum of its parents."""
    args = ["show", *LOG_OPTS, "--remerge-diff", "--format=", *DIFF_OPTS]
    return parse_numstat(git_text(core, *args, "--numstat", merge), git_text(core, *args, "--name-status", merge))


def merges_in(core, *revs):
    """[(sha, parents, subject)] of the merge commits in revs (git rev-list syntax), newest first."""
    out = []
    for line in git_text(core, "log", *LOG_OPTS, "--merges", "--format=%H%x09%P%x09%s", *revs).splitlines():
        sha, parents, subject = line.split("\t", 2)
        out.append((sha, parents.split(), subject))
    return out


def first_parent_commits(core, rng):
    for line in git_text(core, "log", *LOG_OPTS, "--first-parent", "--format=%H%x09%P%x09%s", rng).splitlines():
        sha, parents, title = line.split("\t", 2)
        yield sha, parents.split(), title


def sub_pr(subject, body):
    """(pr or "-", cherry-pick source sha or "-") of one non-merge commit inside a merge."""
    m = SUB_PR_RE.search(subject) or PR_RE.search(subject)
    cherry = CHERRY_RE.findall(body)
    return ((m.group(1) or m.group(2)) if m else "-", cherry[-1] if cherry else "-")


def backport_commits(core, parents, sha):
    """The commits of a backport merge as [(sha, pr, source, subject)] (newest first), or
    None when the merge is not one: it has other than two parents (a plain commit titled "Merge #N",
    an octopus merge, whose remerge-diff git does not make), a merge inside it is an octopus, or no
    commit in it is a cherry-picked develop merge."""
    if len(parents) != 2:
        return None
    parent = parents[0]
    if any(len(p) != 2 for _, p, _ in merges_in(core, f"{parent}..{sha}")):
        return None
    log = git_text(core, "log", *LOG_OPTS, "--no-merges", "--format=%H%x00%s%x00%b%x01", f"{parent}..{sha}")
    rows = []
    for rec in log.split("\x01"):
        if rec.strip():
            c, subject, body = rec.lstrip("\n").split("\x00", 2)
            pr, source = sub_pr(subject, body)
            rows.append((c, pr, source, subject))
    if not any(SUB_PR_RE.search(subject) for _, _, _, subject in rows):
        return None
    return rows


def run(args):
    core = os.path.expanduser(args.core)
    for path in args.report or []:
        if not os.path.isfile(path):
            print(f"error: --report file not found: {path}", file=sys.stderr)
            return 2
    old = git_text(core, "rev-parse", "--verify", args.old + "^{commit}").strip()
    new = git_text(core, "rev-parse", "--verify", args.new + "^{commit}").strip()
    base = git_text(core, "merge-base", old, new).strip()
    os.makedirs(os.path.join(args.out, "packets"), exist_ok=True)
    notes = []
    if base != old:
        notes.append(f"{args.old} is not an ancestor of {args.new} (merge base {base[:10]}). The file list is the "
                     f"direct diff between the pins; commits only in {args.old} are listed with side=old and need "
                     f"the same review.")

    # The authoritative diff-stat: what differs between the two pins.
    files = numstat(core, old, new)
    file_prs = collections.defaultdict(list)

    new_side = list(first_parent_commits(core, f"{old}..{new}"))
    old_side = list(first_parent_commits(core, f"{new}..{old}")) if base != old else []

    # Commits that reached the old pin outside the new pin's first-parent range
    # (release-branch backports), to flag PRs that are probably not deltas.
    oldest_parent = new_side[-1][1][0] if new_side else base
    release_range = (old, "^" + oldest_parent)
    release_pids = patch_ids(core, *release_range)
    release_refs = set(DASH_REF.findall(git_text(core, "log", *LOG_OPTS, "--format=%s%n%b", *release_range)))
    old_side_touch = collections.Counter()
    for line in git_text(core, "log", *LOG_OPTS, "--no-merges", "--name-only", "--format=", *release_range).splitlines():
        if line:
            old_side_touch[line] += 1
    new_pids = patch_ids(core, f"{old}..{new}") if old_side else {}

    # The old pin's tree in a private index file, to reverse-apply PR diffs
    # against it. The repository itself is only read.
    index = os.path.abspath(os.path.join(args.out, "old-pin.index"))
    index_env = dict(os.environ, GIT_INDEX_FILE=index)
    git(core, "read-tree", old, env=index_env)

    def reverse_applies(parent, sha, paths):
        diff = git(core, "diff", *PATCH_OPTS, "--binary", parent, sha, "--", *paths)
        if not diff:
            return False
        check = subprocess.run(["git", "-C", core, *GIT_PINS, "apply", "--cached", "--check", "-R"], input=diff,
                               env=index_env, capture_output=True)
        return check.returncode == 0

    prs = []

    def add_row(side, pr, label, sha, title, parent, stats, via="-", source="-", residual=False):
        """One prs row: `stats` is the numstat of the change the row stands for (the commit's
        diff against `parent`). A residual row stands for a merge's own edits, not for the
        commits inside it: it lists none and has no patch-id signals."""
        areas = collections.Counter()
        add = dele = 0
        reviewable = []
        tests = []
        for path, (a, d, _) in stats.items():
            area, tier = classify(path)
            if path in files:
                tag = label if side == "new" else "old:" + label
                if tag not in file_prs[path]:
                    file_prs[path].append(tag)
            if path.startswith("test/functional/"):
                tests.append(path)
            if tier == "review":
                areas[area] += a + d
                add += a
                dele += d
                reviewable.append((path, a, d))
        sub = [] if residual else git_text(core, "log", *LOG_OPTS, "--no-merges", "--format=%h %s", f"{parent}..{sha}").splitlines()
        pids = {} if residual else patch_ids(core, f"{parent}..{sha}")
        signals = []
        if side == "new":
            matched = sum(1 for pid in pids if pid in release_pids)
            if pids and matched == len(pids):
                signals.append("yes")
            elif reviewable and not residual and reverse_applies(parent, sha, [p for p, _, _ in reviewable]):
                signals.append("tree")
            if pr in release_refs:
                signals.append("ref")
            if matched and matched < len(pids):
                signals.append(f"partial:{matched}/{len(pids)}")
        else:
            matched = sum(1 for pid in pids if pid in new_pids)
            if matched:
                signals.append(f"in-new:{matched}/{len(pids)}")
        net = [f for f in reviewable if f[0] in files]
        tier = "skip" if not reviewable else ("review" if net else "no-net-delta")
        primary = areas.most_common(1)[0][0] if areas else "-"
        prs.append(dict(side=side, pr=pr, label=label, sha=sha, title=title, tier=tier,
                        in_old="+".join(signals) or "no", primary=primary, packet=PACKET_OF.get(primary, "-"),
                        areas=",".join(a for a, _ in areas.most_common()), add=add, dele=dele, net=len(net),
                        files=sorted(reviewable), tests=sorted(tests), sub=sub, via=via, source=source))

    split = []
    for side, commits in (("new", new_side), ("old", old_side)):
        for sha, parents, title in commits:
            m = PR_RE.search(title)
            pr = (m.group(1) or m.group(2)) if m else "-"
            label = pr if pr != "-" else sha[:10]
            subs = None if args.no_split else backport_commits(core, parents, sha)
            if subs is None:
                add_row(side, pr, label, sha, title, parents[0], numstat(core, parents[0], sha))
                continue
            for c, spr, source, subject in subs:
                add_row(side, spr, spr if spr != "-" else c[:10], c, subject, c + "^", numstat(core, c + "^", c),
                        via=label, source=source)
            # What each merge (the backport merge and every merge inside it) itself changed on top of its
            # parents (a conflict resolution, an edit), even in a file a sub-PR also touched: it is in no
            # sub-PR's row. A merge with its own "Merge #N" title is that PR's row, any other is the umbrella's.
            for msha, mparents, msubject in [(sha, parents, title)] + [
                    mg for mg in merges_in(core, f"{parents[0]}..{sha}", *([f"^{old}"] if side == "new" else []))
                    if mg[0] != sha]:
                own = merge_own_numstat(core, msha)
                if own:
                    m = SUB_PR_RE.search(msubject)
                    mpr = m.group(1) if m and msha != sha else pr
                    add_row(side, mpr, mpr if mpr != "-" else msha[:10], msha, msubject + " (changes made by the merge itself)",
                            mparents[0], own, residual=True)
            split.append((side, label, len(subs), sum(1 for _, spr, _, _ in subs if spr == "-")))

    # Every differing file must be attributed. A file whose net change only
    # shows up through a merge resolution (a release branch merged back) is
    # attributed to the release-side or range commits that touched it, or
    # reported as unattributed.
    orphans = []
    for path in files:
        if file_prs.get(path):
            continue
        touched = ["old:" + h for h in git_text(core, "log", *LOG_OPTS, "--format=%h", *release_range, "--", path).split()]
        touched += ["log:" + h for h in git_text(core, "log", *LOG_OPTS, "--format=%h", f"{base}..{new}", "--", path).split()]
        if touched:
            file_prs[path] = touched
        else:
            orphans.append(path)

    def write(name, text):
        with open(os.path.join(args.out, name), "w", encoding="utf-8") as f:
            f.write(text)

    write("prs.tsv", "side\tpr\tsha\ttier\tin_old\tprimary\tareas\tadd\tdel\tnet_files\tvia\ttitle\n" + "".join(
        f"{p['side']}\t{p['pr']}\t{p['sha'][:10]}\t{p['tier']}\t{p['in_old']}\t{p['primary']}\t{p['areas']}\t"
        f"{p['add']}\t{p['dele']}\t{p['net']}\t{p['via']}\t{p['title']}\n" for p in prs))
    write("files.tsv", "path\tarea\ttier\tstatus\tadd\tdel\tprs\told_side_commits\n" + "".join(
        f"{path}\t{classify(path)[0]}\t{classify(path)[1]}\t{files[path][2]}\t{files[path][0]}\t{files[path][1]}\t"
        f"{','.join(file_prs.get(path, []))}\t{old_side_touch.get(path, 0)}\n" for path in sorted(files)))
    write("commits.tsv", "sha\tpr\ttitle\tvia\tcherry_picked_from\n" + "".join(
        f"{h}\t{p['label']}\t{t}\t{p['via']}\t{p['source']}\n"
        for p in prs for h, _, t in (s.partition(" ") for s in p["sub"])))

    for packet in PACKETS:
        lines = []
        for p in prs:
            if p["tier"] == "skip" or p["packet"] != packet:
                continue
            flags = ("" if p["side"] == "new" else " OLD-ONLY") + ("" if p["via"] == "-" else f" via #{p['via']}") + ("" if p["in_old"] == "no" else f" IN-OLD-PIN?={p['in_old']}")
            lines.append(f"#{p['label']} {p['sha'][:10]} +{p['add']}/-{p['dele']} [{p['areas']}]{flags} {p['title']}")
            for path, a, d in p["files"]:
                lines.append(f"    {path} +{a}/-{d}" + ("" if path in files else "  (no net change between pins)"))
            for path in p["tests"]:
                lines.append(f"    test: {path}")
            lines += [f"      * {s}" for s in p["sub"]]
            lines.append("")
        write(f"packets/{packet}.prs.txt", "\n".join(lines) + "\n")
        write(f"packets/{packet}.files.txt", "".join(
            f"{path}\t{files[path][2]}\t+{files[path][0]}/-{files[path][1]}\tPRs:{','.join(file_prs.get(path, []))}"
            f"\told-side:{old_side_touch.get(path, 0)}\n"
            for path in sorted(files) if classify(path)[1] == "review" and PACKET_OF.get(classify(path)[0]) == packet))

    uncovered = []
    coverage = os.path.join(args.out, "coverage.tsv")
    if args.report:
        text = "\n".join(open(path, encoding="utf-8").read() for path in args.report)
        cited = collections.Counter(DASH_REF.findall(text))
        rows = []
        for p in prs:
            n = cited.get(p["pr"], 0) if p["pr"] != "-" else text.count(p["sha"][:10])
            rows.append(f"{p['side']}\t{p['label']}\t{p['tier']}\t{p['packet']}\t{n}\t{p['title']}\n")
            if p["tier"] != "skip" and n == 0:
                uncovered.append(p)
        write("coverage.tsv", "side\tpr\ttier\tpacket\tcitations\ttitle\n" + "".join(rows))
    elif os.path.exists(coverage):
        os.remove(coverage)

    tiers = collections.Counter(p["tier"] for p in prs)
    signals = collections.Counter(s.split(":")[0] for p in prs if p["tier"] != "skip" and p["side"] == "new"
                                  for s in p["in_old"].split("+") if s != "no")
    packet_count = collections.Counter(p["packet"] for p in prs if p["tier"] != "skip")
    file_area = collections.Counter(classify(path) for path in files)
    out = [f"# Pin delta inventory {args.old} ({old[:10]}) -> {args.new} ({new[:10]})\n"]
    out += [f"> WARNING: {n}\n" for n in notes]
    out.append(f"- first-parent commits: {len(new_side)} new-side, {len(old_side)} old-only; review {tiers['review']}, "
               f"no-net-delta {tiers['no-net-delta']}, skip {tiers['skip']}")
    out.append(f"- reviewable new-side PRs with an already-in-{args.old} signal: "
               + ", ".join(f"{k} {signals[k]}" for k in ("yes", "tree", "ref", "partial"))
               + " (verify each against the old pin's source)")
    out += [f"- backport merge #{label} ({side}) split into {n} rows, {direct} direct (no PR title)"
            for side, label, n, direct in split]
    out.append(f"- non-merge commits: {git_text(core, 'rev-list', '--count', '--no-merges', f'{old}..{new}').strip()} "
               f"new-side (`git rev-list --no-merges old..new`); commits.tsv lists "
               f"{sum(len(p['sub']) for p in prs)} per-merge entries, including release-branch commits "
               f"that merges of the old pin bring in")
    out.append(f"- files that differ: {len(files)}; unattributed: {len(orphans)}\n")
    out.append("| packet | reviewable PRs |\n|---|---|")
    out += [f"| {packet} | {packet_count[packet]} |" for packet in PACKETS]
    out.append("\n| tier | area | files |\n|---|---|---|")
    out += [f"| {tier} | {area} | {n} |" for (area, tier), n in sorted(file_area.items(), key=lambda kv: (kv[0][1], kv[0][0]))]
    if args.report:
        out.append(f"\n## Coverage against {', '.join(os.path.basename(path) for path in args.report)}\n")
        out.append(f"Reviewable PRs not cited by the report: {len(uncovered)}\n")
        out += [f"- #{p['label']} [{p['packet']}] {p['title']}" for p in uncovered]
    if orphans:
        out.append("\n## Unattributed files\n")
        out += [f"- {path} ({classify(path)[1]})" for path in orphans]
    summary = "\n".join(out) + "\n"
    write("summary.md", summary)
    print(summary)
    return 1 if uncovered or any(classify(path)[1] == "review" for path in orphans) else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--core", required=True, help="Dash Core checkout (read-only use)")
    ap.add_argument("--old", required=True, help="older pin (tag or commit)")
    ap.add_argument("--new", required=True, help="newer pin (tag or commit)")
    ap.add_argument("--out", required=True, help="output directory")
    ap.add_argument("--no-split", action="store_true", help="keep one row per first-parent commit "
                    "(do not split backport merges into their cherry-picked PRs)")
    ap.add_argument("--report", nargs="+",
                    help="classified report files (markdown); a PR any of them cites as #NNNN counts as covered")
    args = ap.parse_args()
    try:
        return run(args)
    except subprocess.CalledProcessError as e:
        stderr = e.stderr.decode("utf-8", "replace").strip() if e.stderr else ""
        print(f"error: {' '.join(e.cmd)} failed: {stderr}", file=sys.stderr)
        return 2
    except Exception as e:  # a parser surprise must never read as exit 1, "coverage gaps"
        print(f"error: {type(e).__name__}: {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
