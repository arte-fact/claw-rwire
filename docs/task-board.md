# Task board — visual async task list

> A board of **task cards**. The user talks to one **concierge**; for work that isn't a quick
> answer, it delegates to a workspace **owner**, which turns the request into **tasks**. Tasks
> **belong to a workspace**; each workspace runs them **one at a time** (in dependency order),
> and **workspaces run in parallel**. On completion the concierge notifies the user in its own
> voice. This is claw's inter-agent-communication answer: **async coordination through a
> visible, per-workspace board**.

## Context

The user talks to a single **concierge** (the one chat agent). For work that isn't a quick
reply — something recurring, multi-step, or belonging to a project — the concierge `ask`s a
**workspace owner**, which runs a short **planning turn** to turn the request into **tasks**.
A scheduler runs them; when they finish the concierge notifies the user. This is the missing
piece (absent in rust-claw): a visible board the team coordinates through.

The organizing idea is that **the workspace is the unit of work** — already the file boundary
and the team, now the **execution unit** too. Each workspace has an **owner** (its domain lead
+ default task assignee) and **member agents** (the team; an agent can be a member of several).
See the [owner + membership revision](agent-workspace-prompt-model.md#revision-planned-with-the-task-board-workspace-owner--multi-workspace-membership).

- a **task belongs to a workspace** and is assigned to one of its **member** agents
  (default: the owner);
- within a workspace, tasks run **sequentially — one at a time** (so the shared files are
  never written concurrently: no collisions, by construction);
- **workspaces run in parallel** with each other (the unit of concurrency);
- a task can be **blocked by** other tasks **in the same workspace** (a DAG); the workspace
  runs its ready tasks in dependency order. A "pipeline" is just a blocker chain.

Settled design decisions:

1. **Creation** — the **concierge** delegates to a workspace **owner** (`ask`), which creates
   the tasks, auto-routing each to a **member** by persona; the **user can reassign or edit**
   any card on the board.
2. **Notification** — on completion the **concierge** runs a **short turn** to phrase the
   result in its own voice, in the global chat (not a templated string).
3. **Dependencies** — tasks may have **blocker tasks** within their workspace; a task is ready
   only once its blockers are `done`.
4. **Concurrency** — **sequential within a workspace, parallel across workspaces** (we start
   here; parallelism *inside* a workspace can come later once isolation is solved).

## The loop

```
 user ──► CONCIERGE ──┬──► replies in prose
                      └──► ask(workspace, request) ──► WORKSPACE OWNER (planning turn)
                                                          └──► create_task ×N ──┐
                                                                                 ▼
   ┌───────────────────────  TASK BOARD — lanes are workspaces  ─────────────────────────┐
   │   website (lane)                      research (lane)            ← run in PARALLEL    │
   │   ▶ #14 build      running            ▶ #20 crawl     running                         │
   │     #15 review     blocked ◄14          #21 digest    blocked ◄20   ⟳ 08:00          │
   │     #16 deploy     blocked ◄15                                                        │
   │   └ one task at a time, in dependency order ┘                                         │
   └───────────────────────────────────────┬───────────────────────────────────────────────┘
                                            │ scheduler tick: per idle lane, pick the next READY task
                                            ▼
                       EXECUTOR = run_turn on the task's agent, IN the workspace
                                            │ status: done · store result · unblock dependents
                                            ▼
                       CONCIERGE runs a short NOTIFY turn (in the global chat)
                                            │
                                            ▼
              "✓ Daily digest — 3 new bugs, 1 urgent: …"   → posted in the user's chat
```

The user only ever talks to the concierge. It answers, or delegates to a workspace owner,
which queues cards into that workspace's lane. Each lane drains sequentially; lanes advance
in parallel.

## Task card (data model)

| Field | Meaning |
|---|---|
| `id` / `title` | card identity + label |
| `workspace` | the **owner** — the lane it runs in, and the files it touches |
| `agent` | assigned executor (a member of the workspace; user-editable) |
| `spec` | the instruction the executor agent runs |
| `kind` / `schedule` | `oneshot` \| `recurrent` (+ cron → `next_run`) |
| `status` | `pending → running → done \| failed` (+ `blocked`, `cancelled`) |
| `blockers` | other tasks in the same workspace that must finish first |
| `origin_chat` | which thread to notify on completion |
| `result` / `last_run` / `next_run` | last output + scheduling clock |

`blocked` is derived: a task is blocked while any blocker isn't `done`. A linear blocker
chain *is* a pipeline; a recurrent task simply re-enters its lane each period.

## Lifecycle

```
   create_task (into a workspace)
       │
       ▼
   ┌► pending ──(lane idle & all blockers done & due if recurrent)──► running ──┬─► done ─► notify
   │     ▲                                                                       └─► failed ─► notify
   │     ├─ has unresolved blockers ......................... shown as `blocked`
   │     ├─ recurrent: on done → set next_run → back to pending
   │     └─ on done → unblock dependents in the same lane
   └─ cancelled (user / agent) ─► terminal
```

## Scheduler

A dedicated thread ticks every N s → `Command::SchedulerTick`. On tick (the bridge owns the
DB), **for each workspace that has no task currently running**:

- compute the lane's **ready set** — `pending` tasks whose blockers are all `done` and, if
  recurrent, whose `next_run <= now`;
- pick one (highest `priority`, then oldest) and **dispatch** it: a worker runs `run_turn` on
  the task's agent **in that workspace's directory** → posts `Command::TaskFinished{ id,
  result, ok }`. (Exactly today's `AgentReply` worker shape.)

At most **one running task per workspace**; workspaces are independent, so N busy workspaces =
N parallel runs. On `TaskFinished`: store the result; `done`/`failed`; **recurrent** → set
`next_run`, back to `pending`; **unblock dependents** in the lane; enqueue a notification; the
next tick pulls the lane's next ready task.

## Notification

On completion the bridge runs a **short notify turn on the concierge** (the user-facing voice):
a small prompt ("A task you delegated finished — here's the result …; tell the user briefly"),
and its prose is posted as a concierge message in the `origin_chat` global thread (+ an
Activity entry). One model call per completion — accepted, for a notification in the
concierge's own voice.

## Tools

**Concierge** (the chat agent):

- `ask(workspace, request)` — delegate to a workspace's **owner**. The owner runs a planning
  turn (below) and returns a short plan; the concierge relays it to the user. `workspace` is an
  enum of workspaces described by their brief.

**Workspace owner** (during its planning turn, scoped to its own workspace):

- `create_task(spec, title, agent?, schedule?, blockers?)` — mints an id, posts
  `Command::CreateTask`, returns "queued #id". `agent` is an **enum of the workspace's members
  described by their persona** (identity prompts double as routing descriptions); omitted →
  the **owner** itself. `blockers` are ids of earlier tasks in the same workspace.
- `list_tasks()` / `cancel_task(id)` — within its workspace.

## UI

Two surfaces (see the [shell IA](ui-information-architecture.md)):

- **Per-workspace** — each workspace's **Tasks section** shows *its* lane: cards with title,
  agent badge, recurrence icon, status, blocker links, last-result snippet, next-run, the
  **running** card pinned on top. Clicking a card opens an **edit drawer** (retitle, rewrite
  the spec, **reassign the agent**, change the schedule, add/remove blockers, cancel) — how the
  user "suggests an agent / edits the task".
- **Global dashboard** — the **all-lanes parallel view**: every workspace's running/queued
  task at a glance.

Both render from the projected `App.tasks`, reusing the card/badge/drawer/action widgets.

## Schema (additive — claw-rwire only)

```sql
CREATE TABLE IF NOT EXISTS tasks (
  id              TEXT PRIMARY KEY,
  workspace_id    TEXT NOT NULL REFERENCES workspaces(id),  -- owner; the lane + file boundary
  agent_group_id  TEXT REFERENCES agent_groups(id),         -- assignee (a member of the workspace)
  title           TEXT NOT NULL,
  spec            TEXT NOT NULL,
  kind            TEXT NOT NULL DEFAULT 'oneshot',           -- oneshot | recurrent
  schedule        TEXT,                                      -- cron (recurrent only)
  status          TEXT NOT NULL DEFAULT 'pending',           -- pending|running|done|failed|cancelled
  origin_chat_id  TEXT REFERENCES messaging_groups(id),
  created_by      TEXT,
  result          TEXT,
  next_run        TEXT,
  last_run        TEXT,
  priority        INTEGER NOT NULL DEFAULT 0,
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tasks_lane ON tasks(workspace_id, status, priority);

-- Intra-workspace dependencies: `task_id` can't start until `blocked_by` is done.
-- (Both tasks must share a workspace — cross-workspace blockers would couple the lanes.)
CREATE TABLE IF NOT EXISTS task_blockers (
  task_id     TEXT NOT NULL REFERENCES tasks(id),
  blocked_by  TEXT NOT NULL REFERENCES tasks(id),
  PRIMARY KEY (task_id, blocked_by)
);
```

## Mapping to claw-rwire

```
DB          tasks + task_blockers (above); a pipeline = a blocker chain in one workspace
BRIDGE      scheduler thread ──every N s──► Command::SchedulerTick
              tick (bridge owns DB): per idle workspace, pick the next ready task → dispatch
              executor = run_turn on the task's agent IN the workspace ──► Command::TaskFinished
              TaskFinished → store + reschedule/unblock + enqueue notify turn
TOOL        concierge: ask(workspace, request);  owner: create_task / list_tasks / cancel_task
PROJECTION  bridge projects App.tasks: Vec<TaskCard> → per-workspace Tasks section + dashboard
NOTIFY      concierge notify turn → message in origin_chat (global thread) + Activity entry
```

## Build order

| # | Step | Notes |
|---|------|-------|
| 1 | DB: `tasks` + `task_blockers` + query module (CRUD, per-lane ready-scan) | additive |
| 2 | Scheduler thread + `SchedulerTick` / `TaskFinished`; one-running-per-workspace | `bridge` |
| 3 | Executor = `run_turn` on the assigned agent in the workspace dir | reuse worker pattern |
| 4 | Owner `create_task` / `list_tasks` / `cancel_task` + concierge `ask` tools | `tools` |
| 5 | Concierge notify turn → message in the global chat | `bridge` |
| 6 | Tasks section (workspace lane) + edit drawer; global dashboard lanes view | `ui` |
| 7 | Recurrence (cron → `next_run`) + blocker-aware ordering | `bridge` |

## Decisions (resolved)

- **Recurrence catch-up** — **skip to the next slot**. If the app was down across a scheduled
  time, don't fire missed occurrences; just recompute `next_run`. (Avoids a burst of stale
  catch-up runs flooding the lanes on boot.)
- **Auto-route** — the **`create_task` tool schema offers the workspace's members as a
  persona-described enum**; the model picks. No bridge routing rules.
- **Assignee scope** — a task's agent **must be a member** of its workspace; membership is the
  file-access boundary, and the `create_task` enum is exactly the team.

## Out of scope (later)

- **Parallelism within a workspace** (needs run isolation — subdir / worktree copy).
- Non-linear visual pipeline editing (the blocker DAG already allows it under the hood).
- Cross-workspace blockers / cross-channel notification.
- SMS notification (web-first, deferred).
