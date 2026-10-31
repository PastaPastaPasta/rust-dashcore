#!/usr/bin/env python3
"""List what each of pasta's open upstream PRs is waiting on from us.

pr_wait.py reports new events once. After a batch is acked, a review handed to
a fixer can sit for hours with no reply to the reviewer, and a PR whose threads
are all answered can wait forever without a re-request. This sweep rechecks the
current state every heartbeat instead.

Flags per open PR authored by AUTHOR:
  UNANSWERED  a human review thread or PR comment whose last word is not ours
  REREQUEST   a human CHANGES_REQUESTED on an older head, every thread answered,
              reviewer not currently requested
  STALE-ASK   a human CHANGES_REQUESTED on the current head (fix not pushed yet)

Usage: pr-review-sweep.py [owner/repo ...]  (default: the manager's base-sdk PRs)
Exit 0 when nothing needs us, 1 when something does, 2 on a gh error.
Uses REST only (GraphQL limits are shared across threads).
"""
import json
import subprocess
import sys
from datetime import datetime, timezone

AUTHOR = "PastaPastaPasta"
DEFAULT_REPOS = ["dashpay/base-sdk"]
BOTS = {"thepastaclaw", "coderabbitai", "github-actions", "copilot-pull-request-reviewer"}


def gh(path):
    out = subprocess.run(["gh", "api", "--paginate", "--slurp", path],
                         capture_output=True, text=True)
    if out.returncode != 0:
        print(f"gh error on {path}: {out.stderr.strip()}", file=sys.stderr)
        sys.exit(2)
    return [item for page in json.loads(out.stdout) for item in page]


def human(login):
    return login != AUTHOR and not login.endswith("[bot]") and login not in BOTS


def age(ts):
    t = datetime.fromisoformat(ts.replace("Z", "+00:00"))
    return f"{(datetime.now(timezone.utc) - t).total_seconds() / 3600:.1f}h"


def sweep_pr(repo, pr):
    n, head = pr["number"], pr["head"]["sha"]
    flags = []

    threads = {}
    for c in gh(f"repos/{repo}/pulls/{n}/comments?per_page=100"):
        threads.setdefault(c.get("in_reply_to_id") or c["id"], []).append(c)
    open_threads = {}
    for root, cs in threads.items():
        last = max(cs, key=lambda c: c["created_at"])
        if human(last["user"]["login"]):
            open_threads.setdefault(last["user"]["login"], []).append(root)
            flags.append(f"UNANSWERED thread {root} by {last['user']['login']} "
                         f"{cs[0]['path']} ({age(last['created_at'])} old)")

    comments = gh(f"repos/{repo}/issues/{n}/comments?per_page=100")
    ours = [c["created_at"] for c in comments if c["user"]["login"] == AUTHOR]
    for c in comments:
        if human(c["user"]["login"]) and not any(t > c["created_at"] for t in ours):
            flags.append(f"UNANSWERED comment {c['id']} by {c['user']['login']} "
                         f"({age(c['created_at'])} old)")

    latest = {}
    for r in gh(f"repos/{repo}/pulls/{n}/reviews?per_page=100"):
        if human(r["user"]["login"]) and r["state"] in ("APPROVED", "CHANGES_REQUESTED", "DISMISSED"):
            latest[r["user"]["login"]] = r
    requested = {u["login"] for u in pr.get("requested_reviewers", [])}
    for who, r in latest.items():
        if r["state"] != "CHANGES_REQUESTED":
            continue
        if r["commit_id"] == head:
            flags.append(f"STALE-ASK {who} CHANGES_REQUESTED on the current head "
                         f"({age(r['submitted_at'])} old): fix not pushed yet")
        elif who not in requested and not open_threads.get(who):
            flags.append(f"REREQUEST {who}: threads answered, head moved since "
                         f"their review ({age(r['submitted_at'])} old)")
    return flags


def main():
    need = False
    for repo in sys.argv[1:] or DEFAULT_REPOS:
        prs = [p for p in gh(f"repos/{repo}/pulls?state=open&per_page=100")
               if p["user"]["login"] == AUTHOR]
        for pr in prs:
            flags = sweep_pr(repo, pr)
            state = "needs us" if flags else "waiting on others"
            print(f"{repo}#{pr['number']} {pr['head']['sha'][:10]} {state}: {pr['title'][:70]}")
            for f in flags:
                print(f"  {f}")
            need |= bool(flags)
    sys.exit(1 if need else 0)


if __name__ == "__main__":
    main()
