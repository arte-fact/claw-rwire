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

> **Status: DONE** (roadmap M2) — `create_form` moved into a toggleable side panel; title/spec
> required; per-task `max_retries` override added to the create + edit forms (the T5 fold-in).

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

## Suggested sequencing

| Order | Item | Why |
|---|---|---|
| 1 | **R4** (syntax highlight) | Foundational — R1/R2 reuse it |
| 2 | **R5** (create side panel) | Small, self-contained UX win; pairs with the just-shipped task controls |
| 3 | **R1** (file tree + md view) | Builds on R4 |
| 4 | **R2** (rich review) | Builds on R4; reuses the detail thread rendering |
| 5 | **R6** (isola sandboxing) | Spike done; **P0/P1 independent** of the UI work — see [roadmap](isola-integration-roadmap.md) |
| 6 | **R3** (reviewable planning) | Largest; reshapes delegation — own design doc first |
