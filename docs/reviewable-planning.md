# Reviewable planning (R3 / roadmap M6) — DESIGN

> Make the workspace owner's **planning step a reviewable task** in the board's first column. Today
> a delegated request is turned into task cards **immediately and invisibly**; this puts a human
> gate in front of that: the owner *proposes* a breakdown, the user **approves / revises / rejects**
> it, and only on approval are the child tasks created and queued.

## DECISION — Option A (breakdown-as-data), shipped

The first cut shipped **Option B** (held `stopped` child tasks). It was later **migrated to Option A**
(the originally-recommended route) because revise left cancelled tombstones, held cards cluttered the
board before approval, and a crash between create-held and gate could orphan children. Option A keeps
the board empty until approval — the proposal is *pure data*.

**Shipped flow:**
```
concierge ask ─► Command::Ask ─► orchestrate::ask
   mint plan_id; create a PLAN task (is_plan=1, parks AWAITING) in column 1
   run owner planning turn ─► propose_tasks([...]) appends a JSON proposal onto the plan task
     (tools/planning.rs → tasks::proposal::append) — NO child rows yet
   ─► Review shows the proposal (tasks::proposal::get → ProposedTask cards)
        ├─ approve ─► tasks::proposal::materialize: create real PENDING tasks (plan_id-tagged),
        │             wire blockers by title; plan task → done
        ├─ reject  ─► tasks::proposal::clear; plan task → rejected   (nothing to undo)
        └─ revise  ─► tasks::proposal::clear; re-run planning with feedback; re-park
   if workspace.auto_approve ─► orchestrate::planned materialises the proposal immediately
                                (plan task → done "auto-approved"), no gate
```

**Key implementation notes (A, as built):**
- `tasks` gains `proposal TEXT` (the plan's JSON breakdown before approval), alongside the existing
  `is_plan` (the gate task) and `plan_id` (a *materialised* task → the plan it came from).
- `propose_tasks([{title, spec, agent?, schedule?, blockers?}])` replaces `create_task` in the
  planning toolbox; `blockers` reference other proposed tasks **by title** (no ids exist yet),
  resolved to real ids at materialise time.
- `next_ready` already **skips `is_plan` tasks**; materialised children are plain `pending` tasks
  gated by the blocker graph, so they run as soon as their blockers finish.
- `auto_approve` is a per-workspace flag (default 0 = always gate); ON also auto-passes per-task
  human sign-off gates (`scheduler::gate_action`), so the workspace runs hands-off end to end.
- Review of a plan renders the **proposal JSON** (title · assignee · blockers · schedule) with the
  existing Approve / Request-changes / Reject.

---

## Current flow (what we're changing)

```
concierge `ask(workspace, request)`  (tools/concierge.rs)
   └─► Command::Ask { workspace, request, origin_chat }      (bridge/mod.rs)
         └─► orchestrate::ask                                 (bridge/orchestrate.rs)
               owner planning turn, PlanningToolbox:
                 create_task ×N  ──► tasks queued immediately (status pending)   ← no review
               └─► Command::AskPlanned ──► orchestrate::planned (re-project + activity log)
```

The owner's `create_task` calls (`tools/planning.rs`) hit the board straight away. There is **no
checkpoint** — the user sees the cards only after they exist (and after the scheduler may already
be running them). R3 inserts a human gate before the children exist.

**What we can reuse.** The Phase-F human gate already exists end-to-end:
- a task parked at a gate has status **`Awaiting`** (`scheduler::park_awaiting`);
- the **Review inbox** lists `Awaiting` tasks (`tasks::list_awaiting`, `bridge/review.rs`) and now
  renders the producer's full run (M4, `ui::run_chain`);
- a verdict (`Command::ReviewVerdict`) → `scheduler::human_verdict` → `route_verdict`
  (**pass / revise / reject**), which already drives "re-run on revise", "terminal on reject".
- the kanban puts a non-terminal task in its `step_position` column — a fresh task at step 0 sits
  in **column 1** (`ui/workspace_tasks::task_column`).

So the gate, the inbox, the verdict routing, and the column-1 placement are all in place. R3 mostly
needs: a **Plan task** that carries a **proposed breakdown**, and an **approve that materializes**
that breakdown into child tasks.

## The core decision: how the proposal lives before approval

### Option A — breakdown-as-data (recommended)

The planning turn **emits a structured proposal** (a list of `{title, spec, agent?, blockers?,
schedule?}`) via a new owner tool `propose_tasks([...])` instead of calling `create_task`. The
proposal is stored as JSON on a **Plan task** that parks at a human gate. **Approve** parses the
JSON and creates the child tasks; **reject** creates nothing; **revise** re-plans with feedback.

- ✅ No half-created tasks — the board stays clean until approval; the proposal is *pure data*.
- ✅ Reject/revise are trivial (nothing to undo).
- ✅ Fits the gate model: the Plan task's "output" *is* the proposal.
- ➖ Needs a new `propose_tasks` tool + a small proposal renderer in Review; the planning prompt
  changes from "create the tasks" to "propose the tasks".

### Option B — held child tasks

The planning turn calls `create_task` ×N as today, but the children are created **`Stopped`**
(held, reusing the M0 state) and tagged with a `plan_id`. A Plan task gates them. **Approve** flips
the held children to `pending`; **reject** cancels them.

- ✅ Reuses `create_task` + the `Stopped` state directly.
- ➖ **Half-created tasks clutter the board** before approval (Stopped cards in every lane).
- ➖ Needs a `plan_id` link column; revise = cancel-held + re-plan (messier); a crash between
  create-held and gate leaves orphans.

**Recommendation: Option A.** The board-cleanliness and trivial reject/revise outweigh the small
amount of new code; it's the honest "propose → approve → create" model.

## Proposed design (Option A, reusing the gate)

Realize the Plan as a **task whose producer is the owner's planning turn**, with a **human
validator** — so the existing loop + gate machinery carries it, and only two new behaviours are
added (emit a proposal; materialize it on approve).

```
concierge ask ─► Command::Ask ─► create a PLAN task in the workspace (column 1), then run it:
   producer run = owner planning turn (PlanningToolbox → propose_tasks)
        output = the proposed breakdown (JSON), recorded as the run's output
   ─► parks Awaiting (human gate)  ──► Review inbox shows the proposed cards
        ├─ approve ─► materialize: create the child tasks from the proposal; Plan task → done
        ├─ revise  ─► re-run the planning turn with the feedback (new proposal); re-park
        └─ reject  ─► Plan task → rejected; nothing created
```

### Data model

A Plan task is an ordinary `tasks` row, marked so the engine treats it as a plan rather than
ordinary work:

- **Marker:** add `is_plan INTEGER NOT NULL DEFAULT 0` to `tasks` (idempotent migration, the
  established pattern). Alternatively a `kind = 'plan'` — but `kind` already means oneshot/recurrent;
  a separate flag is cleaner. The plan's `spec` holds the original request; its run output (the
  `propose_tasks` payload) holds the proposal JSON.
- **Proposal JSON:** `[{ "title", "spec", "agent"?, "blockers"? (indices into this list), "schedule"? }]`.
  Stored as the producer run's output (already persisted in `task_runs`), so no extra column.
- **No `plan_id` on children** is needed in Option A (children are created fresh on approve, like
  any task); blockers within the proposal are resolved to real ids at materialization time.

### Surface changes

| Area | Change |
|---|---|
| `tools/planning.rs` | New `propose_tasks(tasks:[…])` tool (the planning turn calls it once); it **records** the proposal rather than creating cards. Keep `list_tasks`; drop `create_task` from the *planning* turn (or keep it for non-gated direct use — see open question). |
| `bridge/orchestrate.rs` | `ask` creates the Plan task (`is_plan`, column 1) and runs the owner planning turn as its producer; the turn's output is the proposal. |
| `bridge/scheduler.rs` | A plan task parks at the human gate after its producer runs (reuse `park_awaiting`). On **approve**, `route_verdict`/`human_verdict` for a plan task **materializes** the proposal → `tasks::create` ×N (+ blockers), then finishes the plan `done`. |
| `db/tasks` | `is_plan` column + migration; a `create_from_proposal` helper (parse JSON → `create` ×N, mapping blocker indices to minted ids). |
| `state.rs` / `ui/review.rs` | `ReviewItem` carries an optional **parsed proposal**; the review card renders the proposed cards (title · assignee · deps · schedule) with the existing Approve / Request-changes / Reject buttons. |
| `ui/workspace_tasks.rs` | A Plan task shows in column 1 with a "Plan" affordance; otherwise reuses the card. |

### Materialization (approve)

On a plan task's **pass** verdict: parse the proposal JSON, then for each proposed task mint an id
and `tasks::create` it (assignee validated against members, schedule validated as cron); resolve
`blockers` (indices into the proposal) to the freshly-minted ids via `tasks::add_blocker`. Mark the
plan task `done`. This reuses the exact validation `PlanningToolbox::create` already does.

## Build order

| # | Step | Notes |
|---|------|-------|
| S0 | **This design doc** + the A/B decision | you are here |
| S1 | `is_plan` column + migration; `propose`/proposal JSON shape; `create_from_proposal` (+ tests) | `db` |
| S2 | `propose_tasks` owner tool (records the proposal) | `tools/planning.rs` |
| S3 | `orchestrate::ask` creates + runs the Plan task; producer output = proposal | `bridge` |
| S4 | Approve materializes the proposal (in `human_verdict`/`route_verdict`); revise re-plans; reject discards | `bridge/scheduler.rs` |
| S5 | Review renders the proposed breakdown; board shows the Plan card in column 1 | `ui` |
| S6 | Tests across the path (proposal round-trip, materialization, blocker mapping) | |

## Open questions

1. **A vs B** — recommend **A** (breakdown-as-data). Confirm before building.
2. **Keep direct `create_task`?** Should the owner ever create tasks *without* the gate (e.g. a
   trivial one-task request), or does **all** delegation now go through a reviewable plan? Simplest
   + most consistent: always plan-then-approve. (A "1 task" plan is still a quick approve.)
3. **Auto-approve escape hatch?** A workspace setting to skip the gate for trusted workspaces
   (revert to today's immediate behaviour)? Defer unless wanted.
4. **Plan task in the pipeline?** A plan task is *not* workspace pipeline work — it shouldn't flow
   through the workspace's configured steps. Model it as a standalone producer→human-gate task,
   bypassing `pipeline::steps_for`. (Confirm in S3.)

## Out of scope

- Editing the proposed breakdown inline in Review before approving (approve-as-is or
  request-changes for now).
- Cross-workspace plans.
