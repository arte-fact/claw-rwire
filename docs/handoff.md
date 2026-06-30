# Handoff — delegation & Studio pipeline hardening

Branch: `feat/workspace-ux-overhaul`. All checks green: `cargo fmt --check`, `cargo clippy
--all-targets -- -D warnings` (0 warnings), `cargo test` (183 passing). No `#[allow]`/`unsafe`.

This round focused on the **delegation → reviewable plan → Studio build/review loop**: making the
flow autonomous when asked, honest about state, and actually responsive to "request changes". Read
this top to bottom before extending any of it.

## What changed (by theme)

### 1. Auto-approve covers the whole delegated flow (R3)
- `auto_approve` (per-workspace) previously only skipped the **plan** gate. It now also auto-passes
  each task's **human sign-off** gate: `scheduler::gate_action` → `auto_pass_gate` records an
  `(auto) pass` validator run and routes the loop like a normal pass.
- Safe by construction: an *agent* reviewer in a loop step still runs; a task the reviewer can't
  pass (`on_exhaust: human`) still parks for a human — auto-approve never ships rejected work.
- Pure decision in `gate_action(validator_kind, auto_approve)` (unit-tested).

### 2. Reviewable planning is **Option A** (breakdown-as-data)
Migrated off the original held-`stopped`-children (Option B). The owner's planning turn now emits a
structured **proposal** instead of creating task rows:
- `tools/planning.rs`: `propose_tasks([{title, spec, agent?, schedule?, blockers}])` appends to the
  plan task's JSON `proposal` column (`db/tasks/proposal.rs`). `blockers` reference other proposed
  tasks **by title**.
- Approve → `proposal::materialize` creates real `pending` tasks (plan_id-tagged) and wires blockers
  by title→id. Reject/revise just `clear` the proposal — **no tombstones, no half-created cards**.
- Auto-approve materialises immediately in `orchestrate::planned`.
- Killed run frictions #10 (tombstones), #11 (gate shows nothing), #12 (stray `stopped` cards).
- See `docs/reviewable-planning.md` (updated to reflect A-as-shipped).

### 3. Least-privilege agent tools
`seed::grant_tools`: Forge = coder tools (`bash/read/write/edit`), Critic = read-only, Scout = coder
+ web (`search/fetch/screenshot/interact`). Previously unconfigured agents silently fell back to
*all* tools.

### 4. Task **archive** replaces restart-on-terminal
"Done is done" — terminal tasks no longer offer Restart (kept for `stopped`). Instead **Archive**
(hide from the board) with **Unarchive** on the detail page.
- `tasks.archived_at` column (+ migration); `tasks::control::{archive, unarchive}`;
  `list_for_workspace` filters `archived_at IS NULL` (covers board **and** dashboard counts).
- `Command::{ArchiveTask, UnarchiveTask}`; UI in `workspace_tasks.rs` + `task_detail.rs`.

### 5. Reviewer-quality frictions #1/#2/#3 (from `docs/studio-run-frictions.md`)
- **#1 path guessing**: validators now get the real file layout (`workspace::list_files` →
  `validator_context`), and `read` resolves a guessed path by unique suffix/basename
  (`coder::find_by_suffix`). Residual DSML control tokens scrubbed from stored output
  (`scheduler::strip_dsml`); well-formed leaked calls were already recovered in `agent/client.rs`.
- **#2 prose verdict**: `parse_verdict` falls back to `parse_verdict_prose` (a `Verdict: <x> — …`
  line) so a good review that skipped the tool isn't read as "no verdict".
- **#3 opaque escalation**: an `on_exhaust: human` park now carries a reason
  (`tasks::set_result`) surfaced in the origin-chat ping + activity; the ping fires on escalation,
  not only the first park.

### 6. "Request changes" actually produces a correction run
Two bugs fixed in the human-gate revise path:
- The Studio **Approve** producer (`SIGNOFF_PROMPT`) was review-only, so re-running it on a change
  request edited nothing. It's now **dual-mode**: review-only on the initial sign-off, but applies
  the change request (edit/build/test, scoped) when feedback is present.
- A human "revise" on a task **escalated from an agent loop** (`on_exhaust: human`) hit neither the
  human-gate case nor the (exhausted) iteration cap, so it re-parked without re-running. Fixed via a
  `by_human` flag threaded into `route_verdict`; decision extracted to the unit-tested
  `revise_reruns(by_human, validator_kind, iteration, max_iterations)`.

## ⚠️ Known caveats / follow-ups

- **Existing DBs keep old seeded prompts.** `seed_showcase` only populates a *fresh* DB, so a
  workspace created before this round still has the old `SIGNOFF` ("Do NOT edit") prompt baked into
  its `pipeline_steps` row. Bug-6's scheduler fix works on any DB, but the dual-mode Approve prompt
  needs a **re-seed** (`rm claw-data/central.db`) or a manual edit of the Approve step in the
  Workflow editor. *Open question:* add an idempotent boot-time refresh of showcase step prompts?
  It would clobber user edits to those steps — deferred pending a decision.
- **`strip_dsml`** removes *all* fullwidth `｜` when DSML framing is detected — safe for code/English,
  would clip the rare legitimate use of that glyph.
- **Restart removed for `failed` tasks too** (uniform "terminal → archive"). If a manual retry path
  for `failed` is wanted, branch it: `failed` → Restart, others → Archive.
- **No "Archived tasks" list view.** Archive is one-way declutter + per-task Unarchive on the detail
  page (reachable by URL). A browsable archive screen (like Archived chats) is a clean follow-up.
- **Auto-approve plan is briefly visible.** An auto-approve workspace still creates the Plan task
  (`awaiting`) for a few seconds before `planned` materialises it to `done "auto-approved"` — a
  transient audit record.

## Where things live
- Delegation entry: `bridge/orchestrate.rs` (`ask` → `launch_planning` → `planned`).
- Loop engine: `bridge/scheduler.rs` (`finish` handler → `gate_action` / `route_verdict` /
  `park_awaiting` / `auto_pass_gate`; pure helpers near the bottom + their unit tests).
- Proposal model: `db/tasks/proposal.rs`. Planning tool: `tools/planning.rs`.
- Seeded showcase (agents, briefs, pipeline, prompts, tool grants): `bridge/seed.rs`.
- Frictions log: `docs/studio-run-frictions.md` (items annotated with their fixes).
