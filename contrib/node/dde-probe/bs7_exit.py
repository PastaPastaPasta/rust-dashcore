"""Exit status of BS7's differential commands.

A run passes only when every node under test ran cases, and no case was a
mismatch or a harness error; a command adds its own failures (uncaught
negative controls). `python3 bs7_exit.py` is the negative control: it runs
each command's `main` with a stubbed runner and checks the exit status.
"""
import contextlib
import json
import os
import sys
import tempfile


def failures(summary, **more):
    """Why a run failed, empty when it passed: per node in `summary`, no
    cases, harness errors or mismatches; then each non-empty `more` value."""
    why = [] if summary else ["no node ran"]
    for node, s in summary.items():
        if not s["cases"]:
            why.append("%s: no cases" % node)
        if s["errors"]:
            why.append("%s: %d harness errors" % (node, s["errors"]))
        if s["mismatches"]:
            why.append("%s: %d mismatches" % (node, s["mismatches"]))
    why += ["%s: %s" % (k, v) for k, v in more.items() if v]
    return why


def check(why):
    """Exits non-zero with the reasons when `why` is not empty."""
    if why:
        sys.exit("FAIL: " + "; ".join(why))


def _exit_status(module, runner, argv):
    """`module.main()`'s exit status with `runner` in place of `run` and no
    bs7-tool process."""
    module.run, module.Tool = runner, lambda: None
    sys.argv = argv
    with open(os.devnull, "w") as quiet, contextlib.redirect_stdout(quiet), contextlib.redirect_stderr(quiet):
        try:
            module.main()
        except SystemExit as e:
            return 0 if e.code in (None, 0) else 1
    return 0


def _selftest():
    os.environ.setdefault("BS7_TOOL", "/nonexistent")
    os.environ["DDE_PAIR"] = "v24"
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import bs7_diff_framing
    import bs7_diff_headers2
    out = tempfile.mkdtemp(prefix="bs7-exit-", dir=os.environ.get("TMPDIR", "/dev/shm"))
    probe = os.path.join(out, "probe.json")
    json.dump({"base_version": "00"}, open(probe, "w"))
    os.environ["BS7_PROBE_JSON"] = probe
    report = os.path.join(out, "out.json")

    def framing(cases):
        def run(node, base, n, seed, tool, results):
            results += cases
        return run

    def headers2(cases, controls=True):
        def run(node, base, seconds, seed, tool, results, ctl):
            results += cases
            if controls is not None:
                ctl[node] = {"prev_hash": {"controls": 2, "failed_as_expected": 2 if controls else 1}}
        return run

    good = {"n": 0, "kind": "ping", "core": "deliver", "base_sdk": "deliver", "match": True}
    bad = dict(good, match=False, base_sdk="drop")
    error = {"n": 1, "error": "handshake"}
    h_good = {"n": 0, "match": True, "why": "accepted"}
    h_bad = dict(h_good, match=False, why="Core read, base-sdk failed")
    expect = [
        ("framing matching", bs7_diff_framing, framing([good]), 0),
        ("framing mismatch", bs7_diff_framing, framing([good, bad]), 1),
        ("framing harness error", bs7_diff_framing, framing([good, error]), 1),
        ("framing no cases", bs7_diff_framing, framing([]), 1),
        ("headers2 matching", bs7_diff_headers2, headers2([h_good]), 0),
        ("headers2 mismatch", bs7_diff_headers2, headers2([h_good, h_bad]), 1),
        ("headers2 harness error", bs7_diff_headers2, headers2([h_good, error]), 1),
        ("headers2 no cases", bs7_diff_headers2, headers2([]), 1),
        ("headers2 control not caught", bs7_diff_headers2, headers2([h_good], controls=False), 1),
        ("headers2 no controls", bs7_diff_headers2, headers2([h_good], controls=None), 1),
    ]
    failed = 0
    for name, module, runner, want in expect:
        argv = [module.__file__, report, "1", "1"]
        got = _exit_status(module, runner, argv)
        ok = got == want
        failed += not ok
        print("%s %s: exit %s, wanted %s" % ("ok  " if ok else "FAIL", name, got, want), file=sys.stderr)
    for f in os.listdir(out):
        os.remove(os.path.join(out, f))
    os.rmdir(out)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    _selftest()
