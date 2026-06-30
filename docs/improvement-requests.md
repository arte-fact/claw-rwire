# Improvement requests (formalized)

Formalized, code-grounded versions of the rough notes in
[../IMPROVEMENTS_IDEAS.md](../IMPROVEMENTS_IDEAS.md). Each request states the problem, where it
sits in today's code, a proposed scope, and acceptance criteria. These are **specs to build
from**, not yet committed plans — sequencing is at the end.

Framework constraints that shape several of these: synchronous `#[handler]`/`#[renderer]`, the
fluent `el(...)` builder, the binary wire protocol, and **no JS/CSS source / no build step** — so
anything "visual" (syntax colour, diffs, rendered views) is produced server-side as styled
`el(...)` trees, not client-side scripts.

---

## R1 — File tree + a view/edit editor for clean Markdown reading

> **Status: DONE** (roadmap M3) — editor has a View/Edit toggle (`.md` rendered via
> `widgets::markdown`, code via `highlight_code`); the browser is an inline expandable tree
> (folder = toggle + select-as-create-target, file = open). 150 tests, clippy/fmt clean.

**Problem.** The Files view edits raw text only; reading a Markdown doc means looking at its
source, and the left pane is a single-directory listing rather than a tree.

**Current state.** `src/ui/files.rs` is a two-pane browser + editor (`browser` at `:91`,
`editor` at `:420`); filesystem ops are `bridge::files` (`ListDir`/`OpenFile`/`WriteFile`/…). The
editor pane is a raw textarea. A Markdown renderer already exists: `crate::ui::widgets::markdown`
(used in chat + task detail). The browser lists one directory at a time (`ListDir`).

**Proposed.**
1. **View/Edit toggle** on the editor pane. For a `.md` file, default to a **rendered** view
   (`widgets::markdown`); a toggle switches to the raw textarea for editing; Save persists.
   Non-Markdown files open straight in edit mode.
2. **File tree** in the left pane: nested, expandable folders (lazy-load children via the existing
   `ListDir` per expanded folder), replacing the flat breadcrumb-only listing — breadcrumb stays
   as the path indicator.

**Acceptance.**
- Opening a `.md` file shows it rendered; one click flips to editable source and back.
- The browser shows a collapsible tree; expanding a folder loads its entries; selecting a file
  opens it in the editor.
- Save still round-trips (`WriteFile`); no regression to create/rename/delete.

**Notes.** Tree state (which folders are expanded) is UI state in `App`. Keep the editor's
view/edit mode in `App` too so it survives re-renders. Effort: **M**. Splits cleanly into
"view/edit toggle" (small) and "tree" (medium) sub-PRs.

---

## R2 — Richer review surface: diff, full output, tool usage, rendered thread

> **Status: DONE** (roadmap M4) — the Review inbox renders the producer's full run via the shared
> `ui::run_chain` widget (tool transcript + rendered/highlighted output), and `edit` calls show a
> coloured `old`→`new` line diff. Whole-file before/after diff (needs a pre-write snapshot) is
> future work.

**Problem.** The review inbox shows only a snippet of the producer's text, so a reviewer can't see
*what actually changed* (files written, commands run) before approving.

**Current state.** `src/bridge/review.rs` builds `ReviewItem { output, … }` from
`scheduler::last_producer_output` (the latest producer text only). `src/ui/review.rs` renders that.
Meanwhile the **task detail** view already renders the full run chain — tool transcript cards +
rendered-Markdown output (`src/ui/task_detail.rs`, `run_card` at `:164`), parsed from the persisted
`task_runs.transcript`. The transcript records `write`/`edit` calls with their `path`/`content`
arguments.

**Proposed.**
1. Make the review surface render the **full run** like the detail thread (reuse `run_card` /
   the transcript rendering) instead of a bare snippet: tool usage, full output, rendered Markdown.
2. **Code diff** for files the run changed: from the transcript's `write`/`edit` entries, compute a
   diff against the file's prior content and render it as a styled block (added/removed lines).
   Server-side diff (a small line-diff), emitted as coloured `el(...)` — no client JS.
3. Rendered Markdown already comes from `widgets::markdown`; ensure the review thread uses it.

**Acceptance.**
- Opening a parked task in Review shows the producer's tool calls, full output (rendered md), and a
  diff of each file it wrote/edited.
- Approve / request-changes / reject still drive `ReviewVerdict` unchanged.

**Notes.** Diffing needs the *prior* content. Options: capture a before-snapshot at write time, or
diff against the current on-disk file at review time (cheaper, slightly less precise after later
edits). Recommend the latter for v1. Depends on R4 to colour code inside the diff/output. Effort:
**M–L** (diff is the bulk).

---

## R3 — The owner's planning step is a first-class, reviewable task (kanban column 1)

> **Status: DONE (Option A).** Delegation creates a reviewable **Plan** task; the owner emits a
> structured **proposal** (`propose_tasks` → `tasks::proposal`, stored as JSON on the plan task — no
> task rows until approval); the Review inbox renders it with Approve (materialise) / Request-changes
> (re-plan) / Reject (discard). **Migrated from the original held-`stopped`-children (Option B)** to
> kill three frictions at once: revise no longer leaves cancelled tombstones (#10), the board stays
> empty until approval so there are no stray `stopped` cards (#12), and the gate shows the actual
> breakdown (#11). The per-workspace **auto-approve** toggle is wired end to end: when ON the owner's
> proposal materialises immediately (`orchestrate::planned`) **and** each task's human sign-off gate
> auto-passes (`scheduler::gate_action` → `auto_pass_gate`, recorded as an `(auto) pass` run), so the
> workspace runs hands-off — while an *agent* reviewer in a loop step still runs, and a task the
> reviewer can't pass still parks for a human (no auto-shipping rejected work). The seeded agents are
> also granted **least-privilege tool permissions** (`seed::grant_tools`: Forge = coder tools, Critic
> = read-only, Scout = coder + web) instead of the implicit all-tools fallback. See
> [reviewable-planning.md](reviewable-planning.md).

**Problem.** The workspace owner's role — analyse a delegated request and create + assign the
tasks — happens invisibly and is **not reviewable**. The user can't see or approve the proposed
breakdown before the child tasks start running.

**Current state.** A concierge `ask` spins up `orchestrate::ask` (`src/bridge/orchestrate.rs:18`):
the owner runs a planning turn with `PlanningToolbox` (`src/tools/planning.rs`) that calls
`create_task` directly, then `AskPlanned` re-projects the lane. The planning turn is **not** a row
on the board and has **no gate** — its child tasks are queued immediately. The human-gate
machinery already exists for tasks (Phase F: `validator_kind = "human"` → `Awaiting` → the Review
inbox, `scheduler::park_awaiting`/`human_verdict`).

**Proposed.** Model planning as a real task that lands in the **first kanban column** and is
reviewable:
1. A delegated request creates a **"Plan: …" task** (assigned to the owner) instead of running an
   ad-hoc turn. Its spec is the request; its step is "analyse → propose task breakdown".
2. The planning run **proposes** a breakdown (titles/specs/assignees/blockers/schedule) as its
   output rather than committing child tasks immediately, then **parks at a human gate**
   (`Awaiting`) — visible in the first column and the Review inbox.
3. On **approve**, the proposed child tasks are created + queued; on **request-changes**, the owner
   re-plans with the feedback; on **reject**, nothing is queued.

**Acceptance.**
- Delegating work produces a reviewable "Plan" card in column 1, not silent task creation.
- The proposed breakdown is visible before any child task runs.
- Approve queues the children; revise re-plans; reject queues nothing.

**Notes.** This reshapes delegation, so it's the largest item. Key design choice: does the planner
emit the breakdown as **structured data** the bridge turns into cards on approve (cleaner gate), or
still call `create_task` but into a **held/`stopped` state** released on approve? The structured
route is cleaner and avoids half-created plans. This composes with the run-control work just landed
(`stopped` state, attempt/epoch). Effort: **L**. Should get its own design doc before building.

---

## R4 — Syntax highlighting for code display

> **Status: DONE** (roadmap M1) — implemented in `../rwire` (`feat/code-highlighting`): a zero-dep
> lexer in `rwire-markdown/src/highlight.rs` feeding coloured spans in the code-block renderer,
> mapped to existing `St` colour tokens. Lights up in claw transparently (chat, task detail,
> review, files) with no claw or theming changes.

**Problem.** Fenced code blocks render as plain monospace; no language colouring, in chat, task
output, the review thread, or the file viewer.

**Current state.** `crate::ui::widgets::markdown` (`src/ui/widgets.rs:13`) renders code as a
monospace `pre` with no tokenisation. No build step / no client JS is allowed.

**Proposed.** Server-side syntax highlighting: tokenise fenced code by language and emit coloured
`el(El::Span)` runs using theme style tokens (so it respects light/dark). Start with a small set of
languages relevant here (Rust, SQL, JSON, bash, Markdown); fall back to plain `pre` for unknown
languages. A lightweight tokenizer (or a vetted dependency — check via the Sonatype guide before
adding) feeds the existing renderer.

**Acceptance.**
- A ```` ```rust ```` block renders with keyword/string/comment colours; an unknown language stays
  plain.
- Works everywhere `widgets::markdown` is used (chat, task detail, review, file view).
- No new build step; colours come from theme tokens.

**Notes.** Foundational for R2 (diff/output colouring) and R1 (md view). Pick the tokenizer
deliberately — bundle size / correctness / license matter. Effort: **M**.

---

## R5 — New-task form in a side panel + refined UX

> **Status: DONE** (roadmap M2) — `create_form` moved into a real right-side overlay `Drawer` (the
> same component the System Map uses for entity creation); title/spec required; per-task
> `max_retries` override added to the create + edit forms (the T5 fold-in). _A friendlier
> recurrence input is still TODO — a preset-picker attempt was reverted as poor UX._

**Problem.** The create-task form is a card pinned to the top of the Tasks view, eating vertical
space above the board, and its UX is basic.

**Current state.** `create_form` (`src/ui/workspace_tasks.rs:239`) renders inline at the top of the
`tasks` view; fields are title / spec / cron / assignee. The board (list or kanban) is below it.

**Proposed.**
1. Move creation into a **side panel / drawer** opened by a "New task" button, freeing the board to
   full width.
2. Refine the form UX: clearer field grouping and labels, inline validation (title+spec required,
   cron validated like `bridge::tasks::create` already does), assignee + schedule affordances, and
   surface the new **`max_retries`** override (ties into R-task-control / plan T5).

**Acceptance.**
- "New task" opens a side panel; submitting queues the task and closes it; the board uses the full
  width otherwise.
- Validation errors show inline rather than silently dropping the submit.

**Notes.** Panel open/closed is `App` UI state. Reuse the existing `CreateTask` command. Natural
companion to plan **T5** (the per-task `max_retries` field already projected on `TaskView`). Effort:
**S–M**.

---

## R6 — Sandbox workspace execution with `arte-fact/isola`

> Spike **done** → [isola-sandbox-evaluation.md](isola-sandbox-evaluation.md). Implementation
> roadmap → [isola-integration-roadmap.md](isola-integration-roadmap.md). Verdict: **go, adopt
> incrementally behind a flag.**

**Problem.** Tasks run **sequentially per workspace** in the shared workspace directory with no
isolation; the agent's `bash` tool can touch anything the host user can. Parallelism within a
workspace is also out of scope "pending run isolation" (see `docs/task-board.md`). The only guard
today is lexical path containment (`src/workspace/path.rs`) — no process/syscall/host isolation.

**Findings (from the spike).** isola is a Rust CLI + `isola-core` **library** that gives each
project a persistent, rootless Ubuntu sandbox via **Linux user namespaces + seccomp** (Lima
micro-VM on macOS). Apache-2.0, authored by this project's owner. The workspace dir is **bind-mounted
live, two-way**; uid 1000 maps back to the host user; **network is shared (not isolated)**. Key
consequences for claw:

- **Only the `bash`/exec tool needs sandboxing.** File tools (`write`/`edit`/`read`) operate on the
  bind-mounted host dir and keep working unchanged; we redirect just `src/tools/exec.rs` through
  `isola-core::SandboxBackend::exec_command`.
- **Not the parallelism unlock by itself.** Two sandboxes for one workspace bind-mount the *same*
  dir, so they'd still collide. isola gives **host-safety**; in-workspace parallelism still needs a
  per-run **worktree**, which composes with isola (worktree + sandbox per run).
- **One upstream gap:** `exec_command` (as the sandbox user) inherits stdio; claw needs **captured
  stdout/stderr as the sandbox user** to feed the model. A small `isola-core` addition (we own it).

**Proposed.** Adopt incrementally behind a `CLAW_SANDBOX=isola` flag, with direct execution as the
fallback — host-safety first (one persistent sandbox per workspace, route only `bash`), then
optionally per-run worktree+sandbox for parallelism. Full breakdown in the roadmap.

**Acceptance.** With the flag on, a workspace's `bash` commands run inside its sandbox (host home/
system untouched); with it off (or no host setup), behaviour is exactly as today. See the roadmap's
per-phase acceptance.

**Notes.** Linux-first (claw's server target ✓). Costs: one-time `sudo` host setup, disk per rootfs,
warm-`exec` latency. Effort: **L** across phases; **S** for the P0 upstream fix. Spike effort was **S**.

---

## R7 — A deterministic build/test gate the Refine loop can't pass over

> Source: live end-to-end run, [studio-run-frictions.md](studio-run-frictions.md) #15. **The
> autonomous Refine gate passed code that doesn't compile, regressing a working build** — only a
> human caught it.

**Problem.** A loop step's reviewer is an LLM. In the live run, *Player movement* shipped a
`monster.rs::update_monsters` with a borrow error (`error[E0382]: borrow of moved value`), the Critic
**passed** it, and it reached the human Approve gate with `cargo build` broken — a regression from a
previously-playable build. Nothing deterministic verified the tree compiles/tests before "pass".

**Current state.** The Refine validator is a read-only agent turn (`src/bridge/scheduler.rs:159` —
"the workspace `read` tool plus the `verdict` tool — no writing"); it routes purely on the model's
`verdict` call (`scheduler.rs:526`, `:552`). There is no build/test step in the loop. The agent
*can* run `cargo` via its `bash` tool during Execute, but it's optional and unverified by the engine.

**Proposed.** An optional **verify command** per pipeline step (or per workspace), run by the engine
(not the model) after the producer and before the verdict can be `pass`:
1. Add a `verify_cmd` to `StepFields`/`pipeline_steps` (e.g. `cargo build && cargo test`, default
   none). Run it through the same exec path as the agent's `bash` (`src/tools/exec.rs`), capturing
   output.
2. A **non-zero exit forces `revise`** regardless of the agent verdict, with the captured output as
   feedback; the agent verdict only applies when the command passes (or none is set).
3. Surface the verify result as a row in the run chain (`src/ui/run_chain.rs`) — green/red with the
   tail of output.

**Acceptance.**
- A step with `verify_cmd` set cannot advance past Refine while the command fails; the failure output
  is the revise feedback.
- Steps with no `verify_cmd` behave exactly as today.
- The run chain shows the verify step and its pass/fail.

**Notes.** This is the single highest-leverage fix — it converts "the reviewer hopefully noticed" into
"the build provably passes". Composes with R6 (run the verify inside the sandbox). Effort: **M**.

---

## R8 — Reviewer robustness with weaker / non-OpenAI-native models

> Source: [studio-run-frictions.md](studio-run-frictions.md) #1–#3. With
> `deepseek/deepseek-v4-flash` the Critic repeatedly produced **no structured verdict**, **failed
> every file read**, and **leaked raw tool-call control tokens** — yet the failures were largely
> invisible.

**Problem.** Three compounding failures observed on the same task:
1. **No verdict tool call.** The Critic wrote a correct decision in prose ("Verdict: revise — change
   `_ => continue` to `_ => return None`") but never called the `verdict` tool, so `parse_verdict`
   returned `None`.
2. **Defaulted-revise burns iterations silently.** A `None` verdict defaults to `revise` with
   feedback `"(the reviewer returned no verdict)"` (`src/bridge/scheduler.rs:620-633`), so two such
   reviews exhaust `max_iterations` and escalate to a human — the board just shows `awaiting` at
   Refine with no real reason.
3. **Leaked tool-call tokens + wrong-path reads.** The Critic's `output` was literally
   `<｜DSML｜tool_calls｜>` (deepseek's native tool framing leaking through the OpenAI-compatible
   endpoint, since claw consumes `completion.tool_calls` — `src/agent/client.rs:71-91`), and its 10
   `read` calls used bare paths (`render.rs`, `lib.rs`, `projects`) that don't exist — all failed,
   so it reviewed nothing. Forge's reads in the same project use correct `projects/…/src/…` paths.

**Proposed.**
1. **Prose-verdict fallback** in `parse_verdict` (`src/tools/validator.rs`): if no `verdict` tool
   call, scan the final message for an explicit `verdict: pass|revise|reject` line before defaulting.
2. **Sanitise leaked tool-call control tokens** (`<｜…｜>`, `<|tool_calls|>`, etc.) out of recorded
   `output`, and treat a turn that emitted only such tokens as a retriable malformed response.
3. **Anchor the validator's file paths.** Give the read-only validator turn the project/workspace
   root as cwd (or auto-resolve bare reads against it) so review reads don't depend on the model
   guessing the prefix.
4. **Surface the escalation reason** at the human gate: when a loop escalates via `on_exhaust`, the
   Review/peek panel should say *why* (e.g. "reviewer returned no usable verdict twice") instead of a
   bare `awaiting`.

**Acceptance.**
- A reviewer that states its decision in prose routes correctly without a tool call.
- Recorded `output` never contains raw tool-call control tokens; an all-garbage turn is retried.
- Validator reads resolve against the project root; a review that reads the changed files succeeds.
- The gate shows the escalation reason.

**Notes.** Most of this is model-agnostic hardening that also helps strong models on a bad turn.
Effort: **M**.

---

## R9 — Plan completeness: dependency validation + a final deliverables check

> Source: [studio-run-frictions.md](studio-run-frictions.md) #4, #16, #10.

**Problem.** Decomposition and review are spec-*local*, so whole-request gaps slip through:
- **Dependency gap.** The *game-loop integration* task was not `blocked_by` the *monster-AI* task,
  so it ran against a stub `monster.rs` (it only worked because the integrator re-implemented the AI
  inline — accidental duplication).
- **Dropped deliverable.** The spec explicitly required `ARCHITECTURE.md`; no task was created for
  it, no agent produced it, no reviewer flagged it — the project finished "Done" without it.
- **Duplicate task churn.** Two identical "Scaffold" tasks were created; one ran, one was left
  `cancelled`.

**Current state.** The owner's planning turn creates child tasks + blockers directly
(`src/tools/planning.rs`, `src/bridge/orchestrate.rs`); nothing validates that the blocker DAG covers
the real data/file dependencies, that every named deliverable maps to a task, or that titles are
unique. R3 already made the plan reviewable — this adds *automated* checks to that gate.

**Proposed.**
1. **Plan lint** before the plan parks for review: flag duplicate titles, and (heuristically) tasks
   whose specs reference files/symbols another task produces but that aren't in its `blocked_by`.
2. **Deliverables checklist** extracted from the original request (e.g. "Deliverables" bullets,
   named files like `ARCHITECTURE.md`, "tests for X"); attach it to the Plan task and re-check it at
   the **final** Approve gate — surface any unmet items rather than silently completing.

**Acceptance.**
- The reviewable plan shows lint warnings (duplicate/likely-missing-dependency) for the human to act
  on before release.
- The final deliverable can't be marked Done with an unmet, explicitly-named deliverable without the
  human seeing the gap.

**Notes.** The checklist can be a simple LLM-extracted list stored on the Plan task; the final check
is a cheap presence test (files exist / tasks closed). Composes with R3. Effort: **M**.

---

## R10 — Surface execution state: timeouts/retries, agent heartbeat, held-vs-paused, plan contents

> Source: [studio-run-frictions.md](studio-run-frictions.md) #7, #11, #12, #13.

**Problem.** Several states are invisible or mislabeled on the board:
- **Silent LLM timeout/retry.** *Player movement*'s Execute stage failed with `request failed: timed
  out reading response` and reverted to `pending` to retry — no card signal; it just looked stuck.
- **No "agent working" heartbeat.** A 6-minute generation looks identical to a hang (spinner only,
  no token/elapsed indicator).
- **`stopped` is overloaded.** Plan-held children are parked as `stopped` (`src/db/tasks/control.rs:87`
  — "release its held children — `stopped` → `pending`"), which renders identically to a
  user-paused task; the first-in-chain child even showed `stopped` with no blocker badge.
- **The Plan gate doesn't show the plan.** The parked Plan task's Overview is just the verbatim
  request ("No runs recorded yet"); the actual breakdown only exists as sibling cards.

**Current state.** `RESPONSE_TIMEOUT = 120s`, `RUN_BUDGET = 180s` (`src/agent/client.rs:20,25`) —
tight for a slow model on a large task. Status vocabulary in `src/db/tasks/control.rs` / `state.rs`.

**Proposed.**
1. A **distinct label/status for plan-held + blocked** ("held" / "blocked — awaiting plan") vs
   user-`stopped`, so "why isn't this running?" is answerable from the card.
2. A **retry/timeout chip** on cards (attempt N, "retrying after timeout") driven by the existing
   `attempt`/failure path (`scheduler.rs:448`).
3. An **"agent working" indicator** with elapsed time / streaming token count while a run is active.
4. The **Plan task Overview renders the proposed breakdown** (titles + blockers + assignees), not
   just the request — so the thing being approved is visible in one place.
5. Consider making `RESPONSE_TIMEOUT`/`RUN_BUDGET` configurable per endpoint for slow models.

**Acceptance.**
- Held/blocked children are visibly distinct from paused ones.
- A timed-out/retrying task shows it on the card.
- The Plan gate shows the decomposition without leaving the card.

**Notes.** Mostly UI + status-vocab; (5) is a small config add. Effort: **M**.

---

## R11 — Approval ergonomics: gate the deliverable, not every leaf; scope the finalize pass

> Source: [studio-run-frictions.md](studio-run-frictions.md) #8, #9.

**Problem.** The seeded `Execute → Refine → Approve(human)` pipeline applies to *every* subtask, so a
5-task project needs ~5 human approvals; a task that escalates at Refine **and** runs its Approve
step parks the human **twice** (observed on *Terminal rendering*). Separately, the Approve step's
Forge "finalize" pass isn't scope-limited — finalizing *Core types* also rewrote `game.rs`/`render.rs`
(files owned by later tasks).

**Current state.** Pipeline steps are per-workspace and inherited by every task (`src/bridge/seed.rs`
seeds Studio's three steps; `src/db/pipeline.rs`). R3 mentions a per-workspace **auto-approve** toggle
as remaining work — this generalises it. The finalize prompt (`FINALIZE_PROMPT` in `seed.rs`) runs a
full producer turn with the normal write tools.

**Proposed.**
1. **Gate policy**: per-workspace/per-task option to **auto-pass leaf/child subtasks** and require a
   human only on the **final deliverable** (or only on tasks flagged `needs_human`). Folds in R3's
   auto-approve toggle.
2. **No double-gating**: if a task escalated to a human at Refine, don't immediately park it again at
   Approve for the same human in the same sitting — collapse to one decision.
3. **Scope the finalize pass** to the task's own files (or make it review-only) so sign-off prep
   doesn't silently edit sibling tasks' deliverables.

**Acceptance.**
- A decomposed project can be configured to need one human approval (the deliverable), not one per
  subtask.
- A task never parks the same human twice in a row for one completion.
- The finalize pass doesn't modify files outside the task's scope (or is review-only).

**Notes.** Composes with R3 and R9. Effort: **M**.

---

## R12 — PWA: show a clear offline state instead of a perpetual "Reconnecting…"

> Source: [studio-run-frictions.md](studio-run-frictions.md) #14. Mostly an `rwire` concern (like R4).

**Problem.** When the browser can't reach the server, the service worker still serves the cached app
shell, which then loops on **"Reconnecting…"** forever with no hint it's an offline/cached copy.
Clearing the SW cache exposed the real `ERR_CONNECTION_REFUSED`. This cost real debugging time.

**Current state.** The SW + shell caching are generated by rwire's PWA module (claw calls
`pwa_config()` in `src/main.rs`; `SHELL=['/', …]` is cached and replayed for navigations). The
reconnect overlay is the capsule client's.

**Proposed (in rwire).** Distinguish "server unreachable" from "reconnecting": when the WS can't
establish for N attempts against a cached shell, show an explicit **"Offline — showing a cached copy"**
banner (with a manual retry) instead of the indefinite reconnect spinner.

**Acceptance.** A served-from-cache page with no reachable server shows an offline banner, not a bare
"Reconnecting…".

**Notes.** Lives in `../rwire` (pwa + capsule client). Effort: **S** (rwire-side). Also worth a
README note for the gotcha: an automation/sandbox browser isolated from `localhost` reproduces this.

---

## R13 — Optimize the seeded Studio pipeline (the step sequence itself)

> Source: live end-to-end run. The frictions in R7/R8/R11 all surface *through* the seeded Studio
> pipeline; this entry redesigns that seed so the default showcase is leaner, provably-correct, and
> needs fewer human gates. Consumer of R7 (verify step) + R11 (gate policy).
>
> **Status: the two prompt-level wins are DONE** (`src/bridge/seed.rs`): the standalone `Execute`
> stage is folded into the `Build` loop's iteration 0 (one Forge produce before the first review,
> not two), and `Approve` is now a **review-only** sign-off (`SIGNOFF_PROMPT` — "Do NOT edit"). The
> agent system prompts were hardened alongside (see "Prompt changes" below). The deterministic
> verify gate (R7) and deliverable-only gating (R11) remain the deeper, engine-side work.

**Problem — walk one task through the seeded pipeline, in order.** Studio seeds three steps
(`src/bridge/seed.rs`): `Execute` (stage, Forge, `:107`) → `Refine` (loop, Forge `REFINE_PROMPT` ⇄
Critic, `max_iterations: 2`, `on_exhaust: human`, `:116`) → `Approve` (loop, Forge `FINALIZE_PROMPT`
⇄ human, `:133`). Every task inherits all three. Sequentially, the cost per leaf task is:

1. **Execute** — Forge produces the initial code → *Forge call #1*.
2. **Refine iter 0** — `REFINE_PROMPT` says "refine the previous step's work", so Forge re-touches the
   code it just wrote → *Forge call #2, frequently a near-no-op* — then Critic reviews → *Critic call*.
   The Critic reviews code **that was never built** (no compile step in the loop).
3. **Refine iter 1** (if `revise`, or if the model emits no `verdict` → it defaults to revise, R8) —
   Forge + Critic again; on exhaust → escalates to a human with no clear reason.
4. **Approve** — Forge runs `FINALIZE_PROMPT` → *Forge call #3, and it mutates files (unscoped — in
   the run it edited sibling tasks' files)* — then parks for a **human**.

So a single leaf task costs **≥3 Forge calls + ≥1 Critic + 1 human gate**; the 5-task game cost ~15
Forge passes and 5 human approvals, with the build never deterministically checked and a weak-model
verdict miss silently burning Refine iterations. Observed directly: redundant double-produce; Critic
passing a non-compiling tree (R7); finalize editing other tasks' files (R11 #9); per-task approval
fatigue + double-gating (R11 #8); silent escalation (R8).

**Proposed — collapse three steps into two, sequenced for correctness.**

**Step 0 — `Build` (loop, agent validator).** Fold today's `Execute` *into the loop's iteration 0*
so there is **one** Forge produce before the first review, not two:
- producer: Forge with a **build/fix prompt** (not the "refine the previous step" preamble) — on
  iter 0 it builds; on later iters the Critic's feedback tells it what to fix, so one prompt covers
  both. (Drops `REFINE_PROMPT`'s "don't start over" framing that only makes sense after a separate
  Execute.)
- **engine verify (R7), positioned before the Critic:** `cargo build && cargo test`. A non-zero exit
  **forces `revise`** with the captured output as feedback — so the Critic only ever reviews a
  green tree, and the loop *cannot* advance a non-compiling build.
- validator: Critic (agent). `max_iterations: 3` (verify + review can legitimately need a couple of
  passes), `on_exhaust: human` — but now the escalation reason is concrete (last verify/critic
  output), per R8 #4.

**Step 1 — `Sign-off` (human gate, deliverable-scoped).**
- **No Forge finalize produce.** Sign-off must not mutate code; if a summary is desired, make it
  **read-only** (no write tools), removing Forge call #3 and the sibling-file edits.
- Human validator, but per **R11** required **only on the final deliverable** — auto-pass leaf
  subtasks once `Build` is green. For the seed showcase, flag the *last* planned task `needs_human`
  so exactly **one** approval lands for the whole project.

**Net per leaf task:** ~1–2 Forge + 1 Critic + 1 engine verify, **0 human gates** (1 for the
deliverable) — down from 3 Forge + 1 human each, and the tree is **provably building/passing tests at
sign-off**.

**Acceptance.**
- A fresh DB (`seed_showcase`) seeds the two-step `Build → Sign-off` pipeline.
- No task is reviewed or signed off while `cargo build`/`cargo test` fails.
- There is no second Forge produce before the first Critic review, and sign-off edits no files.
- Building the showcase needs **one** human approval (the deliverable), not one per subtask.

**Notes.** Order of operations: land **R7** (engine verify step) and **R11** (gate policy +
`needs_human`/auto-pass) first, then this is a small edit to `src/bridge/seed.rs` (swap the three
`pipeline::insert*` calls for two, new build prompt, drop `FINALIZE_PROMPT`'s write tools). **Available
now without R7/R11:** the two cheap wins are (a) **drop the standalone `Execute` stage** and let the
loop's iter 0 build (removes the always-redundant second produce), and (b) make the `Approve`
finalize **review-only**. The deeper single-worker serialization (one task at a time per workspace,
`docs/task-board.md`) is orthogonal and tracked under R6's worktree note. Effort: **S** (seed edit)
on top of R7/R11.

**Prompt changes (done, alongside the seed edit).** Hardened the system prompts against the live
failure modes (`src/agent/mod.rs` role defaults + `src/bridge/seed.rs` personas/step prompts):
- `EXECUTOR_PROMPT` / `FORGE_PERSONA` — verify your own work (build + test before claiming done),
  never claim a file written without writing it, stay scoped to the task's files.
- `VALIDATOR_PROMPT` / `CRITIC_PERSONA` — read files at their real workspace-relative paths and
  retry on a failed read; require evidence the build/tests passed before `pass`; **always** record
  the decision via the `verdict` tool, never only in prose.
- `BUILD_PROMPT` (new, self-contained — a step prompt *replaces* the role default, doesn't layer)
  covers iteration 0 (build) and later iterations (fix per feedback). `SIGNOFF_PROMPT` (new) is
  review-only. Guarded by two regression tests in `seed.rs`.

---

## Suggested sequencing

R1–R6 are the original UI/infra set (mostly DONE). R7–R12 come from the live end-to-end Studio run
([studio-run-frictions.md](studio-run-frictions.md)) and target **correctness of the
agentic loop** — they're the priority for trustworthy autonomous output, especially with weaker
models.

| Order | Item | Why |
|---|---|---|
| 1 | **R7** (build/test gate) | Highest leverage — stops the loop signing off on non-compiling code; a working build was regressed without it |
| 2 | **R8** (reviewer robustness) | Makes the Critic actually review with weaker models (verdict fallback, token sanitising, correct read paths, visible escalation) |
| 3 | **R4** (syntax highlight) | Foundational — R1/R2 reuse it (DONE) |
| 4 | **R5** (create side panel) | Small, self-contained UX win (DONE) |
| 5 | **R10** (surface exec state) | Mostly UI; removes the "is it stuck?" ambiguity (timeouts, heartbeat, held-vs-paused, plan contents) |
| 6 | **R1** (file tree + md view) | Builds on R4 |
| 7 | **R2** (rich review) | Builds on R4; reuses the detail thread rendering |
| 8 | **R9** (plan completeness) | Catches dependency gaps + dropped deliverables; composes with R3 |
| 9 | **R11** (approval ergonomics) | Cuts approval fatigue; folds in R3's auto-approve toggle |
| 10 | **R13** (optimize the seed) | Consumes R7+R11 — flips the default Studio pipeline to `Build(verify) → Sign-off`; two cheap wins available now |
| 11 | **R6** (isola sandboxing) | Spike done; **P0/P1 independent** of the UI work — see [roadmap](isola-integration-roadmap.md) |
| 12 | **R3** (reviewable planning) | Largest; reshapes delegation — own design doc first |
| 13 | **R12** (PWA offline state) | Small; lives in `../rwire` |
