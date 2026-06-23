# claw-rwire roadmap

A single sequence for the [improvement requests](improvement-requests.md) (R1–R6) plus the
in-flight task-control work. Two requests already have their own detailed plans, linked below; the
rest are broken into PR-sized subtasks here. Each subtask lands independently green on the gate:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Framework constraints that shape the UI items: synchronous `#[handler]`/`#[renderer]`, the `el(...)`
builder, **no JS/CSS source / no build step** — so all "visual" work (syntax colour, diffs, rendered
views) is produced server-side as styled `el(...)` trees.

## Dependency map

```
task-control (T1–T4 shipped) ─► T5 max_retries ─┐
                                                 ▼
M1  R4 syntax highlight ─► M3 R1 file tree + view/edit
        │                                        
        └────────────────► M4 R2 rich review ─► M6 R3 reviewable planning
M2  R5 create side panel ◄── (folds task-control T5)
M5  R6 isola P0/P1  ──(independent track, runs alongside M1–M4)──► M7 isola P2 + worktrees
```

Read it as: **R4 is the visual foundation** (R1 and R2 reuse it); **R2 feeds R3** (the plan review
reuses the rich-review surface); **isola is an independent track**; **task-control just needs
closing out**.

## Milestones

| M | Theme | Items | Effort | Detail |
|---|---|---|---|---|
| **M0** | Close out task control | task-control T5 / T7 / T8 | M | [task-control-and-restart-plan.md](task-control-and-restart-plan.md) |
| **M1** ✅ | Visual foundation | R4 syntax highlighting | M | below — **done** (in `../rwire`) |
| **M2** ✅ | UX quick win | R5 create side panel (+ T5 field) | S–M | below — **done** |
| **M3** | Files | R1 file tree + view/edit editor | M | below |
| **M4** | Review depth | R2 rich review (tools + diff + thread) | M–L | below |
| **M5** | Sandboxing | R6 isola P0 + P1 | L | [isola-integration-roadmap.md](isola-integration-roadmap.md) |
| **M6** | Delegation | R3 reviewable planning step | L | below (design doc first) |
| **M7** | Parallelism (stretch) | isola P2 + per-run worktrees | XL | isola roadmap P2 |

M5 (isola) has no UI dependency and can proceed in parallel with M1–M4. M0 is mostly done already.

---

## M0 — Close out task control

The run-control feature (Stop/Start/Restart, auto-retry, attempt isolation) is implemented on
`feat/task-control-restart`. Remaining, per [the plan](task-control-and-restart-plan.md):

- **T5** — retry config: the per-task `max_retries` field in the edit drawer + global defaults
  editor. **Fold the drawer field into M2** (the create/edit form rework); keep the global-defaults
  editor here.
- **T7** — "ask owner to revise" (`revise_task` tool + `AskRevise` command).
- **T8** — cooperative hard-stop (cancel flag checked in the agent round loop, `client.rs:66`).

**Done when.** T5/T7/T8 land green; the feature branch is ready to merge.

---

## M1 — R4: syntax highlighting (visual foundation) — **DONE**

**Goal.** Fenced code blocks render with token colours everywhere `widgets::markdown` is used
(chat, task detail, review, files). Server-side only — no JS/build step.

**Implemented in `../rwire` (branch `feat/code-highlighting`), not claw** — the highlighting lives
in the framework, so every rwire app (claw included) gets it transparently with **zero claw
changes**. It also needed **zero theming changes**: the `St::TextSuccess`/`TextWarning` tokens and
the `--o`/`--p` (green/amber) theme vars already existed.

**Decision (with the user): in-house lexer, no syntect.** The Files editor (R1) targets the
**project stack** (Rust/SQL/JSON/shell/TOML/YAML), so the zero-dep lexer is proportionate; syntect's
breadth wasn't worth its build-step + dep weight + fixed-(non-theme)-colour cost. Crucially, the
highlighter is **reusable for plain code files, not just markdown** — `highlight_code` is public.

| # | Subtask | Outcome |
|---|---|---|
| S1 | Tokenizer | In-house, **zero-dep** table-driven lexer at `rwire-markdown/src/highlight.rs` (Rust/JSON/bash/SQL/TOML/YAML; unknown → flat fallback). Hard invariant: tokens reassemble the input exactly. |
| S2 | Render | Public `code::highlight_code(code, lang) -> ElementBuilder` builds the styled block with coloured spans; the markdown parser **and** standalone callers (the R1 file viewer) share this one path. |
| S3 | Colours | Mapped to existing `St` tokens: keyword→`TextAccent`, string→`TextSuccess`, number→`TextWarning`, comment→`TextMuted`, plain→inherit. No new tokens/vars. |
| S4 | Tests | 12 tokenizer tests (round-trip, escapes, unicode, case-insensitive SQL, TOML/YAML, fallbacks) + parser & `code` integration tests. rwire-markdown gate green (58 tests); claw's 146 green against the updated dep. |

**R1 hook:** claw's Files editor (M3) will call `rwire_markdown::highlight_code(content, lang)`
with the language inferred from the file extension — no markdown round-trip.

**Done when.** A ```` ```rust ```` block renders coloured in chat and task detail; unknown languages
unchanged; gate green.

---

## M2 — R5: new-task creation in a side panel (+ task-control T5 field) — **DONE**

**Goal.** Move task creation out of the always-on top card into a side panel, refine the form, and
surface the `max_retries` override.

| # | Subtask | Outcome |
|---|---|---|
| S1 | Panel state | `App.new_task_panel_open` + `toggle_new_task`; a "New task" button in the board toolbar (accented while open). |
| S2 | Side drawer | `create_form` rendered in a real **right-side** overlay `rwire_components::Drawer` (backdrop + header close), matching the System Map's entity-creation drawer; opened by the toolbar "New task" button, closed on submit/backdrop/X. |
| S2b | Schedule picker | Replaced the raw cron box with a friendly **Repeat** preset select (hourly / daily 08:00 / weekdays / weekly / monthly / Don't repeat) + a **Custom (cron)** override box; `resolve_schedule` (pure, unit-tested) maps the pair to a cron string. Shared by create + edit (edit preselects the matching preset, else Custom with the cron prefilled). |
| S3 | Validation | title/spec are HTML-`required` (native inline gating); cron/assignee/empty still surface via the bridge error banner. |
| S4 | T5 fold-in | `max_retries` plumbed end-to-end: `NewTask`/`create`, `update`, bridge `Create`/`Edit` structs, `CreateTask`/`UpdateTask` commands, and a shared `retries_input` in both forms. Blank = global default; negatives ignored. |
| S5 | Tests | `create_persists_a_max_retries_override`, `update` round-trips `max_retries`. 147 tests green; clippy/fmt clean. |

**Note.** The *global* defaults editor (`task.max_retries` / `task.retry_base_secs` on the Prompts/
Settings view) is the remaining half of T5 — left under **M0**; the engine already reads those
settings with code defaults (2 / 30s).

**Done when.** "New task" opens a side panel; submit queues + closes; validation shows inline; a
per-task retry budget can be set and persists; gate green.

---

## M3 — R1: file tree + view/edit editor

**Goal.** A nested, expandable file tree and an editor that *renders* Markdown (view) with a toggle
to raw (edit). Builds on M1 for nicer in-view code.

Current state: `src/ui/files.rs` shows a single dir (`App.files_entries`) and edits raw text in a
`Textarea` (`editor` at `files.rs:420`, `OpenFile` content). No tree, no view mode.

| # | Subtask | Files | Notes |
|---|---|---|---|
| S1 | Editor **view/edit toggle**: `App.files_editor_mode` (`View`/`Edit`); `.md` opens in `widgets::markdown` view, toggle to the `Textarea`; non-md opens in edit | `src/ui/files.rs`, `src/state.rs` | Save still uses `WriteFile` |
| S2 | **Tree** read-model: `App` holds expanded dir paths + their entries; expanding a folder issues `ListDir` for it and merges results | `src/state.rs`, `src/bridge/files.rs` | stay lazy — load children on expand only |
| S3 | Render the nested tree in the browser pane (replace the flat listing; keep the breadcrumb) | `src/ui/files.rs:91` (`browser`) | |
| S4 | Tests: tree expand/collapse + path handling (reuse `workspace::path` guards) | `bridge/files.rs` / `state` | |

**Done when.** Opening a `.md` shows it rendered with a one-click flip to source; the browser is a
collapsible tree; create/rename/delete unregressed; gate green.

---

## M4 — R2: richer review (tools + diff + rendered thread)

**Goal.** The Review inbox shows the producer's **full run** — tool usage, rendered-Markdown output,
and a **code diff** of changed files — not just a snippet + a "View run chain →" link
(`review.rs:43`). Builds on M1 for code colour.

| # | Subtask | Files | Notes |
|---|---|---|---|
| S1 | Extract the run-chain rendering (tool cards + md output) from `task_detail.rs` (`run_card`) into a shared widget | new `src/ui/run_chain.rs`, `src/ui/task_detail.rs` | both detail + review render through it |
| S2 | `ReviewItem` carries the latest run's transcript (not just `output`); review card renders the shared widget | `src/state.rs`, `src/bridge/review.rs`, `src/ui/review.rs` | reuse `last_producer_output`'s run lookup |
| S3 | **Code diff:** from the run's `write`/`edit` transcript entries, line-diff vs the current on-disk file; render as a coloured block (uses M1 colour) | new `src/diff.rs` (small) or vetted dep (Sonatype), `src/ui/run_chain.rs` | diff-vs-current for v1 (no before-snapshot) |
| S4 | Tests: diff unit tests; `ReviewItem` projection includes the transcript | `diff.rs`, `bridge/review.rs` | |

**Done when.** A parked task in Review shows tools, rendered output, and a diff per changed file;
verdict buttons unchanged; gate green.

---

## M5 — R6: sandbox execution with isola (parallel track)

Full breakdown in **[isola-integration-roadmap.md](isola-integration-roadmap.md)**. Summary:

- **P0 (upstream, isola repo):** add `exec_command_captured` (as the sandbox user → `(exit,
  output)`); benchmark warm-`exec` latency.
- **P1 (claw, behind `CLAW_SANDBOX=isola`):** route only the `bash` tool through the workspace's
  sandbox via `isola-core`; file tools stay host-side; lazy ensure-sandbox with graceful fallback;
  destroy on workspace delete; gated integration test proving a stray write can't escape to the host.

No UI dependency — can run alongside M1–M4. **Done when** the flag sandboxes shell execution and
flag-off behaviour is byte-for-byte today's.

---

## M6 — R3: the owner's planning step becomes a reviewable task

**Goal.** Delegation produces a **reviewable "Plan" task** in kanban column 1 instead of silently
creating child tasks (`orchestrate::ask` → `PlanningToolbox.create_task` today). The largest item —
**write a design doc first** (it reshapes delegation). Benefits from M4's review surface.

| # | Subtask | Files | Notes |
|---|---|---|---|
| S0 | Design doc: breakdown-**as-data** (bridge creates cards on approve) vs **held/`stopped`** child tasks released on approve | `docs/` | decide before building; data route is cleaner |
| S1 | Planning turn **proposes** a structured breakdown (titles/specs/assignees/blockers/schedule) as output, not direct `create_task` | `src/bridge/orchestrate.rs`, `src/tools/planning.rs` | |
| S2 | Model the plan as a task in column 1 that parks `Awaiting` (human gate) carrying the proposal | `src/bridge/scheduler.rs` (`park_awaiting`/gate), `src/db/tasks/*` | reuse the Phase-F human gate |
| S3 | On **approve** → create + queue children; **revise** → re-plan with feedback; **reject** → nothing | `scheduler::human_verdict`/`route_verdict` | composes with task-control (`stopped`, `attempt`) |
| S4 | UI: plan card shows the proposed breakdown; review surface (M4) renders it for approval | `src/ui/review.rs`, board | |
| S5 | Tests across the new path | | |

**Done when.** Delegating work yields a reviewable plan card; the breakdown is visible before any
child runs; approve queues children, revise re-plans, reject queues nothing; gate green.

---

## M7 — Parallelism (stretch)

isola **P2**: per-run **git worktree** (or copy) mounted into a per-run sandbox, relax the
one-task-per-workspace lane (`scheduler::tick`/`running_in`), and define a reconcile/merge strategy
back to the canonical tree. Depends on M5 proven; needs its own design doc (the merge problem is the
hard part). See the isola roadmap's P2.

---

## Consolidated sequencing

| Order | Milestone | Why here |
|---|---|---|
| 1 | **M0** task-control finish | already in flight; close it |
| 2 | **M1** R4 syntax highlight ✅ | foundation for M3/M4 — done |
| 3 | **M2** R5 create panel ✅ | small win; carries task-control T5 — done |
| 4 | **M3** R1 files tree/view | builds on M1 |
| 5 | **M4** R2 rich review | builds on M1; feeds M6 |
| — | **M5** R6 isola P0/P1 | **parallel** with 2–5 (no UI dep) |
| 6 | **M6** R3 reviewable planning | largest; after M4; design doc first |
| 7 | **M7** parallelism | stretch; after M5 |

**Cross-cutting:** check any new dependency through the Sonatype guide before adding (repo policy);
keep new files under the ~300-line cap (e.g. `highlight.rs`, `run_chain.rs`, `diff.rs` are
deliberately separate modules); every subtask ends on a green gate.
