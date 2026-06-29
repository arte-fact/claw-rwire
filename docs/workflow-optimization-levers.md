# Workflow optimization & simplification levers

Grounded in a **live end-to-end run** (2026-06-29): a fresh-seeded Studio workspace was
given a 3-part coding roadmap ("quotes" Python CLI → pytest suite → README) and driven
through the UI: concierge → owner planning turn → R3 plan gate → approve → Execute →
Refine (Critic) → Approve (human) → serial unblock of dependents.

What actually happened, and where the levers are. This is **not** a call to become Jira —
Jira's workflow editor complexity is a cautionary tale. The goal is to keep claw's simple
linear pipeline and add the *few* levers that matter for **autonomous coding agents**.

---

## What the live run proved works

- The owner's planning turn produced a **correct dependency DAG** (core → tests → readme)
  and the R3 plan gate rendered it cleanly for approval.
- Forge wrote **real, working Python** (argparse CLI + JSON store), and **self-tested it via
  `bash`** (`python3 quotes.py add/list/random`, empty-file cases, all exit 0).
- The **run chain / live activity** view (per-tool transcript with exit codes) is genuinely
  better than a Jira ticket history — it shows *how* the work was done, not just status.
- Approving #1 immediately unblocked #2 — the blocker scheduler works.

## What the live run exposed (each → a lever below)

| Observation in the run | Lever |
|---|---|
| #2/#3 sat `pending` until #1 was **fully done** — even though #1's code already existed on disk, and #2/#3 were the only work; one lane, one at a time | **O1** parallelism via isolation; **O2** DAG-aware scheduling |
| Critic **passed `quotes.py` having only `read` it** — the reviewer cannot run pytest or the file | **O3** machine/test gate; **O4** executable review |
| Forge burned ~6 tool calls probing a non-existent `/workspace` cwd before `pwd` revealed the real path | **O5** tell the agent its workdir |
| `bash` ran `cd /tmp/claude-1000/.../studio` — the **absolute host path**, unsandboxed | (security seam — tracked: bwrap/isola) |
| Planner created a README card, **cancelled it, recreated it** — blind to the repo, left a `cancelled` duplicate cluttering CLOSED | **O7** grounded planning; **S5** card hygiene |
| Pipeline editing **locked** the entire time tasks were in flight | **S6** finer-grained lock |
| Review offered only approve / reject / request-changes — no inline tweak of the plan | **S3** inline plan edit |
| `priority` never set; ordering was creation-time only | **O8** wire priority/rank |

---

## Optimization levers (throughput · cost · quality)

Ranked by leverage for the coding-agent use case.

- **O1 — Intra-workspace parallelism via per-run worktree/subdir.** Today a workspace is a
  single sequential lane (`tasks::running_in` gate). Independent siblings serialize; a
  human Approve gate is a hard stop for the whole lane. Give each run an isolated working
  copy (git worktree) so N independent cards run at once, then merge. *This is the keystone
  — it also fixes the accidental unsafe concurrency on the review-resume path.* Tracked as
  isola P2 (M7 stretch); merge strategy is the hard part. **Jira:** unlimited WIP, optional
  WIP limits per column.

- **O2 — DAG-aware scheduling, not single-file drain.** `next_ready` picks one card; a
  diamond (`A → {B,C} → D`) runs fully serial even though B,C are independent. With O1,
  schedule ready *branches* concurrently. Also consider **artifact-ready** unblock: #2 only
  needed #1's *code* (already on disk), not #1's *task status*. **Jira:** "blocks" links are
  advisory; nothing forces serial execution.

- **O3 — A machine/test validator step (`validator_kind = command`).** Add a step kind that
  runs a command and passes iff exit 0 (e.g. `pytest -q`, `cargo test`, `npm test`). Turns
  the producer's ad-hoc self-tests into an **objective gate** and a real Definition of Done.
  Cache the result on the run. **Jira:** CI status checks / required checks before transition.

- **O4 — Give the agent validator `bash` (read-only-ish exec).** Even keeping `agent`
  validators, let Critic *run* the build/tests instead of only `read` + `verdict`. Today the
  gate blesses code it never executed. Pair with O3 for objective + judgment review.

- **O5 — Tell every run its absolute workspace path and that relative paths work.** One line
  in the system prompt eliminates the `/workspace` probing tax (latency + tokens) seen live.
  Cheap, immediate.

- **O6 — Context compaction between steps/iterations.** `step_context` re-injects *all*
  prior stage/producer outputs every revise; long pipelines balloon the prompt. Summarize or
  diff-only the carried context. Pure cost/latency win.

- **O7 — Grounded planning: give the planning toolbox `read`/`bash`.** The owner decomposes
  coding work without ever seeing the repo (`PlanningToolbox` has only create/list/cancel).
  Letting it inspect the tree yields better splits and fewer wasted/duplicate cards.
  **Jira:** backlog grooming against the actual codebase/PRs.

- **O8 — Wire `priority`/rank end-to-end.** The column exists and `next_ready` orders by it,
  but nothing ever sets it ≠ 0 and `create_task` has no priority arg → no backlog ordering.
  Add a priority arg + drag-to-rank. **Jira:** priority field + rank.

- **O9 — Diff-based review.** Show the reviewer (and the Approve gate) the *change*, not just
  the producer's prose + a re-read. Partially scoped in R2 (edit-call diffs); extend to
  whole-file before/after via a pre-write snapshot. **Jira+Bitbucket/GitHub:** PR diff review.

- **O10 — Per-step retry policy.** Retry budget is whole-task; a flaky test step can't have
  its own policy. (Tracked: out of scope today.)

## Simplification levers (less friction · smaller surface · clearer model)

- **S1 — Pipeline templates / presets.** Instead of hand-building steps, offer a small
  library ("Build + Test + Review", "Doc", "Spike/throwaway", "Hotfix") to apply in one
  click. Keeps the simple model while covering the common shapes. **Jira:** issue types +
  workflow schemes — but *avoid* Jira's arbitrary-graph editor complexity.

- **S2 — Per-card / per-type fast-track.** `auto_approve` is per-workspace and all-or-nothing.
  Allow skipping the human Approve gate for low-risk card types (docs, chores) while keeping
  it for risky ones. Cuts human round-trips.

- **S3 — Inline plan editing in Review.** Today: approve / reject / request-changes only — a
  full re-plan round-trip to fix one card. Let the user edit/delete/add a proposed child in
  place before approving. (Tracked: out of scope.) **Jira:** edit issues inline on the board.

- **S4 — Make "blocked" visible.** A failed/rejected/cancelled blocker leaves dependents
  `pending` **forever and invisible** (`next_ready` requires blocker `status = 'done'`). Add
  a Blocked lane/badge and a cascade option. Removes a silent-stall failure mode. **Jira:**
  flagged/blocked indicators + linked-issue status.

- **S5 — Card hygiene for planner self-corrections.** The planner's `cancel_task` left a
  duplicate `cancelled` README in CLOSED. Hide planner-internal cancellations, or collapse
  the CLOSED column by default.

- **S6 — Finer-grained pipeline-edit lock.** Editing locks while *any* task is in flight;
  a queued task (not yet started) and downstream steps not yet reached needn't lock. Lets you
  tune a pipeline without draining the whole board.

- **S7 — A QA/tester teammate (or just O3/O4).** All three cards — including the *test*
  task — were assigned to Forge; Critic only reviews. There is no role that runs tests. O3/O4
  largely subsume the need; otherwise a "Tester" persona whose step is the `command` gate.

---

## How claw compares to Jira (and what to borrow vs. avoid)

| Capability | claw today | Jira | Verdict |
|---|---|---|---|
| Board / kanban | Pipeline steps *are* the columns | Statuses mapped to columns | claw simpler; fine |
| Workflow customization | Per-workspace **linear** stage/loop | Arbitrary status graph + transitions, conditions, validators, post-functions | **Keep claw simple**; add presets (S1), not a graph editor |
| CI / test gates | agent or human verdict, **no execution** | Required status checks, automation rules | **Borrow** (O3/O4) |
| Parallel work | **1 per lane** | Unlimited WIP (+ optional limits) | **Borrow** via isolation (O1) |
| Dependencies | Same-workspace blockers, **serial** | Cross-project issue links (advisory) | **Borrow** DAG + cross-workspace (O2) |
| Hierarchy | plan → children (1 level) | Epic → story → subtask (+ initiatives) | Add 1 more level if needed; don't over-build |
| Priority / rank | field exists, **unused** | Priority + drag rank | **Borrow** (O8) |
| Sprints / velocity / estimation | none | Story points, sprints, burndown, velocity | Repurpose as **token/cost budgets** per run, not ceremony |
| Blocked visibility | none (silent stall) | Flags + linked status | **Borrow** (S4) |
| History / audit | **Per-tool run-chain transcript** | Changelog + comments | **claw wins** — keep it |
| Does the work | **Agents write & run code** | Tracks humans doing it | **claw's whole point** — Jira can't |

**The framing:** Jira *tracks* work; claw *performs* it. Don't chase Jira's configurability —
its workflow editor is the canonical example of PM-tool over-engineering. Borrow the handful
of concepts that make autonomous agents reliable: **isolation-backed parallelism (O1),
objective test gates (O3/O4), DAG scheduling (O2), grounded planning (O7), ranked backlog
(O8), and visible blocked state (S4)**. Everything else, keep linear and simple.

## Suggested sequencing

1. **Cheap wins now:** O5 (workdir hint), O8 (priority), S4 (blocked visibility), S5 (card
   hygiene). Low risk, immediately better.
2. **The quality unlock:** O3 + O4 (test/command gate + executable review) — turns "passed"
   into "green".
3. **The throughput unlock:** O1 → O2 (per-run worktrees → DAG scheduling). Biggest change,
   biggest payoff; also closes the unsafe review-resume concurrency window.
4. **Polish:** O6 (compaction), O7 (grounded planning), O9 (diff review), S1–S3, S6.
