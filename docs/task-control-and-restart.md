# Task control & auto-restart — DRAFT

> Give a task a **resting, editable state** and real **user controls** over its run: **stop**,
> **start/resume**, **restart**. Make a failed run **auto-retry** (bounded, with backoff)
> instead of dying. While a task is stopped, the user can **edit its spec directly** or **ask
> the workspace owner to revise it** (a short planning turn). Closes the gaps that the board
> has today: no retry, no pause, a "cancel" that doesn't actually stop the worker, and an edit
> button on a running task that silently does nothing.

## Why now — what's missing today

| Gap | Where | Effect |
|---|---|---|
| One-shot failure is terminal | `scheduler.rs:481` | A transient endpoint blip kills the task forever |
| Recurrent failure is made terminal on purpose | `reschedule_at` `scheduler.rs:401` | "Don't hammer the lane" — but the task just stops, silently |
| No "stop"; **cancel is soft** | `tasks::cancel` `tasks.rs:184`, worker `scheduler.rs:187`, `finished` `scheduler.rs:424` | The orphan worker keeps running, and its late result can overwrite `Cancelled` → `Done`/`Failed` (no status guard) |
| Edit on a **running** task is a no-op | button at `workspace_tasks.rs:326`, messages frozen at dispatch `scheduler.rs:111` | The user edits, nothing changes, the old run lands |
| No resting state to edit safely | `TaskStatus` `tasks.rs:13` | Only terminal or in-flight — nowhere to pause-and-fix |

Two facts the design exploits so it stays small:

- `next_ready` scans **only** `status='pending'` (`tasks.rs:224`) → a new idle status is skipped
  by the scheduler with **zero query changes**.
- the ready-scan already gates on `next_run IS NULL OR next_run <= now` (`tasks.rs:228`) →
  **backoff retries reuse the recurrence clock**: set `next_run = now + backoff`, the next tick
  picks it up. No new scheduling path.

## Status model

Add one non-terminal status. The terminal set is unchanged.

```
TaskStatus = Pending | Running | Awaiting | Stopped*  (idle, lane-free)
           | Done | Failed | Rejected | Cancelled      (terminal)
            * new
```

- **`Stopped`** — paused by the user. Idle (frees the lane), invisible to `next_ready`, **editable**.
  The resting state from which Start / Restart / edit / ask-owner all happen.
- **`Cancelled`** stays *terminal* — "I'm done with this card." Distinct from Stopped ("hold it").

### Transitions (the user verbs)

```
                 ┌──────── stop ────────┐
 pending ───────►│                      │
 running ───────►│      STOPPED         │── start  ──► pending  (resume at current step)
 awaiting ──────►│   (idle, editable)   │── restart ──► pending  (fresh from step 0)
                 └──────────────────────┘── cancel ──► cancelled
       done│failed│rejected│cancelled ──── restart ──► pending  (fresh from step 0)
       pending ─── start now (optional) ─► clears backoff, bumps priority, runs next tick
```

- **Stop** (`pending`/`running`/`awaiting` → `stopped`): pause. For a *running* task this also
  **abandons the in-flight run** (below) and frees the lane immediately.
- **Start / Resume** (`stopped` → `pending`): re-queue, **continue from the current
  `step_position`** with the existing run chain as context. (A run dropped mid-step is simply
  re-launched at that step.)
- **Restart** (`stopped` *or any terminal* → `pending`): clean slate — `step_position = 0`,
  `iteration = 0`, `auto_retries = 0`, `attempt += 1`, clear `result`/`next_run`. The new
  `attempt` isolates context from prior runs (see *Attempt isolation*).
- **Cancel** (existing, hardened): now also abandons the in-flight run via the run guard.

## Auto-restart on fail

A run that fails for an **execution** reason retries automatically, bounded and backed off,
before going terminal.

**What counts as retryable** (vs. a deliberate terminal outcome):

| Outcome | Source | Retry? |
|---|---|---|
| Run error (`Err` from `run_turn_streamed`, prep failure, `ok=false`) | `scheduler.rs:436`, `:206` | **Yes** — transient by nature |
| Validator `reject` | `route_verdict` `scheduler.rs:535` | No — the work was reviewed and refused |
| Loop `on_exhaust = fail` | `scheduler.rs:553` | No — re-running just exhausts again |
| Human `reject` from the inbox | `human_verdict` `scheduler.rs:656` | No |

So auto-retry hooks **only** the execution-failure branches of `finished()` (Stage/Producer
`!ok`). Rejections and exhaustion stay terminal and use the manual **Restart** path instead.

**Mechanism** — resume at the failed step, don't start over:

```
run fails (ok=false, retryable)
   │
   ├─ auto_retries < max_retries ─► auto_retries += 1
   │                                 status = pending
   │                                 next_run = now + backoff(auto_retries)   ← reuses next_ready gate
   │                                 (keep step_position, keep run chain)
   │                                 → next tick re-launches launch_current at the SAME step
   │
   └─ auto_retries == max_retries ─► one-shot:  terminal Failed + notify origin chat
                                      recurrent: resume cron (next_run = next slot), NOT terminal
```

- The failed run is still recorded (`ok=false`) as audit, but `step_context` already filters to
  `run.ok` (`scheduler.rs:253`), so a failed attempt **never pollutes** the retry's context.
- `backoff(n)` = exponential: `base * 2^(n-1)`, capped (proposed `base = 30s`, cap `1h`).
- **Recurrent improvement:** today a recurrent failure is terminal (`reschedule_at` returns
  `None`); with a bounded budget it can retry then *resume its schedule* instead of dying.

**Config** — a global default in `settings` (like the role preambles, `bridge/mod.rs:1054`),
overridable per task:

- `settings["task.max_retries"]` (default `2` → 3 total tries), `settings["task.retry_base_secs"]` (`30`).
- per-task `max_retries` (nullable column; `NULL` → the global default), editable in the drawer.

## Stopping a live run — the run guard

The worker is a bare `std::thread` with no handle (`scheduler.rs:187`). We can't kill it; we can
**make its result a no-op** and (phase 2) ask it to bow out early.

### Phase 1 — epoch guard (drop the stale result; free the lane now)

Add `run_epoch INTEGER NOT NULL DEFAULT 0` to `tasks`.

- A single `dispatch` helper **bumps `run_epoch`**, reads it, and threads it through `spawn_run`.
- `Command::TaskFinished` carries the `epoch` the worker was launched at (`bridge/mod.rs:236`).
- `finished()` (`scheduler.rs:424`) **first checks** `epoch == task.run_epoch`; if not, the run
  was superseded (stopped, cancelled, restarted, or re-dispatched) → record nothing, route
  nothing. Return.
- **Stop / Cancel / Restart bump `run_epoch`** → any in-flight worker's result is dropped on
  arrival. The lane is freed immediately (status leaves `running`); the orphan thread runs to
  completion in the background and its output is discarded.

This also **fixes the soft-cancel bug**: a cancelled task's late result can no longer overwrite
`Cancelled`.

> Caveat: phase 1 frees the lane but the orphan still finishes its current turn — including any
> file writes it makes. That's acceptable for "stop caring about this run." Hard interruption is
> phase 2.

### Phase 2 — cooperative hard-stop (bound the wasted work)

The agent loop is `for _ in 0..MAX_TOOL_ROUNDS { … }` (`client.rs:66`) — a natural checkpoint.

- The bridge keeps a registry `HashMap<task_id, Arc<AtomicBool>>` of cancel flags.
- `dispatch` creates a flag, hands the `Arc` to the worker; `run_turn_streamed` checks it at the
  **top of each round** and breaks early (answers/stops without another model call or tool call).
- Stop sets the flag → the worker halts after at most one in-flight round/tool call, bounding
  spend and preventing further file writes.

Phase 1 ships the UX; phase 2 is a follow-up that makes stop *cheap*, not just *clean*.

## Editing a stopped task

Direct edit is the existing drawer (`workspace_tasks.rs:386`) — made **safe and honest**:

- The **Edit** affordance is shown only when the task is **not running** (`pending`, `stopped`,
  or a terminal task being prepped for restart). On a `running` task the slot shows **Stop**
  instead — removing the silent-no-op footgun.
- A stopped task's edited `spec`/`title`/`agent`/`schedule`/`max_retries`/`blockers` are picked
  up by the next **Start** (resume, same step) or **Restart** (fresh) — because the run
  re-builds its messages at launch (`scheduler.rs:111`).

## Asking the owner to revise the spec

When the user would rather have the domain lead refine the spec than write it themselves, route
it through the **owner planning turn** — mirroring the existing `ask` → owner → `create_task`
flow (`orchestrate.rs:18`, `tools/planning.rs`).

- New command `Command::AskRevise { task_id, instruction, origin_chat }`.
- The handler resolves the task's workspace owner and runs a short planning turn seeded with the
  task's **current title + spec** and the **user's instruction**, with a one-tool toolbox:
  - `revise_task(id, spec?, title?, schedule?)` — scoped to the workspace, **refused if the task
    is `running`** (the bridge enforces; a stopped/pending task is the target). Updates the row
    via `tasks::update` (`tasks.rs:324`) and re-projects.
- The owner reads current + instruction, decides the new wording, calls `revise_task`, confirms
  in a sentence (relayed like `AskPlanned`, `orchestrate.rs:80`).

UI: on a stopped card, an **"Ask owner to revise"** disclosure (a small textarea → `AskRevise`).

## Attempt isolation (Restart context)

So a **Restart** doesn't inherit the prior attempt's outputs as "work produced so far":

- Add `attempt INTEGER NOT NULL DEFAULT 0` to `tasks` and to `task_runs`. Restart bumps
  `tasks.attempt`; appended runs stamp the current attempt.
- The scheduler's context read (`task_runs::list_for_task`, used at `scheduler.rs:111`) becomes
  **attempt-scoped** (`list_for_task_attempt`) so `step_context` only sees the current attempt.
- The **detail view** (`task_detail.rs` via `build_detail`) still lists *all* runs, grouped by
  attempt with a divider — the full history of why earlier attempts failed.
- Auto-retry does **not** bump `attempt` (it resumes the same logical work at the same step);
  only manual **Restart** does.

## Schema (additive — claw-rwire only)

```sql
ALTER TABLE tasks ADD COLUMN run_epoch    INTEGER NOT NULL DEFAULT 0;  -- dispatch guard
ALTER TABLE tasks ADD COLUMN auto_retries INTEGER NOT NULL DEFAULT 0;  -- consumed auto-retries
ALTER TABLE tasks ADD COLUMN max_retries  INTEGER;                     -- NULL → global default
ALTER TABLE tasks ADD COLUMN attempt      INTEGER NOT NULL DEFAULT 0;  -- manual-restart generation
ALTER TABLE task_runs ADD COLUMN attempt  INTEGER NOT NULL DEFAULT 0;  -- which attempt produced it
-- new status value 'stopped' — no schema change (status is TEXT)
```

Idempotent `add_*_column` migrations in `schema.sql` (matching the `agent_memory.paused`
precedent, `schema.sql:313`). `TaskStatus::from_str` already defaults unknown → `Pending`, so an
old row reading back is safe.

## Surface changes

**`db/tasks.rs`** — `stop`, `resume`, `restart`, `note_retry(id, next_run)`, `bump_epoch`/
`current_epoch`; extend `Task` + `from_row`/`SELECT` with the four new columns. New
`list_for_task_attempt`.

**`bridge/mod.rs`** — `Command::{StopTask, StartTask, RestartTask, AskRevise}`; add `epoch` to
`TaskFinished`; cancel-flag registry (phase 2). Keep `CancelTask`.

**`bridge/scheduler.rs`** — `dispatch` helper bumps epoch + (phase 2) registers a cancel flag;
`finished()` opens with the epoch guard; failure branches route through `retry_or_fail`;
`reschedule_at` lets a recurrent task resume cron after exhausting retries.

**`tools/planning.rs`** (or a small `revise` toolbox) — `revise_task`; `orchestrate.rs` gains an
`ask_revise` entry point alongside `ask`.

**`state.rs` / UI** — `TaskView` gains `attempt` / `auto_retries` / `max_retries` / a
`next_retry` hint. Cards show status-appropriate buttons:

| Status | Buttons |
|---|---|
| running | **Stop** · Cancel |
| pending | Start now* · Edit · Stop · Cancel |
| stopped | **Start** · **Restart** · Edit · *Ask owner to revise* · Cancel |
| awaiting | Approve / Request changes / Reject (existing) · Stop |
| done/failed/rejected/cancelled | **Restart** |

The detail view (`task_detail.rs`) gains the same action bar plus a retry line
("attempt 2 · auto-retry 1/2 · next 14:03") and attempt dividers in the run chain.

## Build order

| # | Step | Module | Notes |
|---|------|--------|-------|
| 1 | `Stopped` status + `stop`/`resume`/`restart` DB fns + migrations | `db` | additive; `next_ready` unchanged |
| 2 | `run_epoch` guard end-to-end (dispatch bumps, `TaskFinished` carries, `finished` drops stale) | `bridge` | also hardens cancel |
| 3 | `StopTask`/`StartTask`/`RestartTask` commands + card/detail buttons | `bridge`,`ui` | edit hidden while running |
| 4 | Auto-retry: `retry_or_fail` + backoff via `next_run`; recurrent resume-cron | `bridge` | unit-test the decision (like `next_action`) |
| 5 | `max_retries` global default + per-task field; retry indicator | `db`,`ui` | |
| 6 | `attempt` isolation (scoped context + grouped detail) | `db`,`bridge`,`ui` | restart is clean |
| 7 | `revise_task` owner tool + `AskRevise` delegation + drawer disclosure | `tools`,`bridge`,`ui` | mirrors `ask` |
| 8 | Phase-2 cooperative hard-stop (cancel flag in the round loop) | `agent`,`bridge` | bounds spend |

## Decisions (proposed)

- **Auto-retry resumes at the failed step**, not from the top — cheaper, correct for transient
  failures; only manual **Restart** starts over.
- **Reject / exhaust are not auto-retried** — they're judgments, not transient errors.
- **Stop is non-terminal; Cancel stays terminal** — two different intents, both kept.
- **Backoff via `next_run`** reuses the recurrence clock — no second scheduling path.
- **Restart preserves prior runs** (attempt-scoped) rather than deleting them — keep the audit.

## Open questions

- Default `max_retries` (proposed **2**) and backoff base/cap (proposed **30s … 1h**)?
- Should **Start now** on a `pending` task (jump the queue + clear backoff) ship in v1, or defer?
- Phase-2 hard-stop: break the round loop only, or also pass the flag into long tool calls
  (e.g. `bash`) for finer interruption?

## Out of scope (later)

- Killing the OS process of an orphaned worker mid-tool-call (phase-2 bounds it at round edges).
- Rolling back files a stopped/failed run already wrote (workspace is shared, sequential).
- Per-step retry policy (today the budget is whole-task).
