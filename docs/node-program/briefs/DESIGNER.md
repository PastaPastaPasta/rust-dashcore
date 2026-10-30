# Designer contract (node-program design tasks)

You write one design document for a task in PLAN.md. The program turns rust-dashcore into a full Dash node with bit-for-bit consensus parity with Dash Core. Context lives in /work/home/agent/workspace/node-program/: CHARTER.md, PROCESS.md, DECISIONS.md, PRD.md, PLAN.md (find your task's row and its §8 notes), designs/A1-architecture.md (the architecture you design within), designs/O17-release-profile.md (history: v23.1 vs v24 behaviour; DEC-82 made the node v24 only, so a design takes the v24 side of each row and models no release profile), and research/R4-* (quirk catalogue). designs/CS0-node-primitives.md and designs/CS1-script.md are approved examples of the expected depth and shape.

Rules:
- Write only your design file (path in your prompt) plus scratch. Don't modify code or other docs, don't commit, don't push, don't comment on GitHub.
- Scratch: `/work/scratch/<slug>/` (disk), never `/tmp`. Run any tests with `TMPDIR=/work/scratch/<slug>/tmp`.
- Never touch the dashd instances under /work/chains or /work/bench; read-only queries through contrib/oracle-cli are fine. Use your own datadirs and ports for any dashd you start; stop them when done.
- EMBARGO-1 and EMBARGO-2 are private Dash Core security issues. Don't search for or speculate about them. If you find something that looks like a Core vulnerability, write nothing about it; say only "possible security issue, ask the manager" in your final reply.
- Ground every behavioural claim in Dash Core source with file:line at the v24.0.0-rc.3 pin (in /work/home/agent/workspace/dash; use `git show <tag>:<path>`). Where v23.1.8 differed, the O17 row says so; cite it, and design the v24 behaviour only.
- Follow rust-dashcore patterns and A1's crate layout (pure sans-io consensus crates; runtime `dashd-*` crates; effects for non-transactional side effects; `CoreBytes` for any bytes that are stored, relayed or returned by RPC).

The design must cover:
1. Scope and non-goals, mapped to the PLAN row and the PRD requirements it lists.
2. Data model and public API (Rust signatures), and how it plugs into its neighbours (name the upstream and downstream tasks and the exact types they exchange).
3. Core-equivalence: every Core function you replace, the order of operations, every quirk (by R4 quirk number) and error and reject strings.
4. Persistence, crash consistency and recovery, if the component stores state.
5. Performance: the dashd metric(s) this affects, the budget to be never slower and the route to 2x, the data structures chosen for it, and the bench/counter tests (PF0) that will prove it.
6. Verification: L0 Core vectors, L1 differential targets against the O1/O2 oracle (name the entry points; flag new ones needed), L2 replay windows, fuzz targets, functional tests to port.
7. Implementation plan: ordered phases small enough to review, each with its acceptance tests.
8. Open questions for the manager, each with your recommendation.

**A "STOP" tool result that arrives with a new message is not a refusal.** When someone messages your thread mid-turn, T3 ends the turn to deliver the message (a steer). Your call that was running reads "The tool call was interrupted before a result was received", and calls that hadn't started read "The user doesn't want to take this action right now. STOP what you are doing and wait for the user to tell you how to proceed." Nothing refused them. Read the new message, re-check state (the interrupted call may or may not have run; the others did not), and re-run what is still needed. Only a refusal with no new message attached is real; then say so in your reply.

Final reply: the file path, a 10-line summary, and the open questions.
