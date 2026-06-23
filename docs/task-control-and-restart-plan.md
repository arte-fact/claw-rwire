# Implementation plan — task control & auto-restart

Companion to [task-control-and-restart.md](task-control-and-restart.md). Eight subtasks, each
PR-sized, TDD, and **independently green** on the gate before the next starts:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Rules in force: no `#[allow]`/`unsafe`, newtypes/enums over strings, exhaustive `match` on domain
enums, regression test per behaviour, ~300-line file soft cap (split before adding).

### Cross-cutting: adding `TaskStatus::Stopped`

Adding the variant breaks every **exhaustive** match on `TaskStatus`. There is exactly one that
must change: `TaskStatus::as_str` (`tasks.rs:26`). `from_str` (`tasks.rs:38`) has a `_` arm — add
an explicit `"stopped"` case before it. UI status checks are string-based
(`task_detail.rs:27`, `workspace_tasks.rs:310`, `:463`), so they compile but must be extended by
hand — listed per subtask. Add `TaskStatus::is_terminal(self) -> bool` (Done/Failed/Rejected/
Cancelled) and use it in the scheduler/bridge where the enum is in hand, so the rule lives in one
place.

### File-size note

`src/db/tasks.rs` is already ~363 source lines (over the soft cap). **Before adding** the
lifecycle fns, convert it to a directory: `src/db/tasks/mod.rs` (the `Task` struct, `TaskStatus`,
CRUD, the ready-scan) + `src/db/tasks/control.rs` (`stop`/`resume`/`restart`/`note_retry`/
`bump_epoch` and their tests). Keep the public path `crate::db::tasks::*` unchanged by re-exporting
from `mod.rs`.

---

## T1 — `Stopped` status + lifecycle DB fns + migrations

**Goal:** the resting state and the row-level operations exist and are tested, with nothing wired
to the UI yet.

**Files:** `src/db/schema.sql`, `src/db/mod.rs`, `src/db/tasks/` (split), `src/db/task_runs.rs`.

**Schema** — add to the `tasks` `CREATE TABLE` (`schema.sql:214`) and `task_runs`
(`schema.sql`), so a **fresh** DB has them:

```sql
-- tasks
run_epoch     INTEGER NOT NULL DEFAULT 0,   -- dispatch guard (bumped on launch/stop/cancel/restart)
auto_retries  INTEGER NOT NULL DEFAULT 0,   -- consumed automatic retries since last manual start
max_retries   INTEGER,                      -- NULL → global default
attempt       INTEGER NOT NULL DEFAULT 0,   -- manual-restart generation
-- task_runs
attempt       INTEGER NOT NULL DEFAULT 0,   -- the task.attempt this run belongs to
```

**Migration** — one new fn called from `migrate()` (`db/mod.rs:77`), mirroring
`add_task_pipeline_columns` (`db/mod.rs:98`) exactly (loop of `ALTER … ADD COLUMN`, treat
`is_duplicate_column` as success):

```rust
fn add_task_control_columns(conn: &Connection) -> rusqlite::Result<()> { /* 4 + 1 ALTERs */ }
```

**`db/tasks` changes:**
- `TaskStatus::Stopped` + `as_str`/`from_str`/`is_terminal`.
- `Task` gains `run_epoch`, `auto_retries`, `max_retries: Option<i64>`, `attempt`. Update the
  `SELECT` const (`tasks.rs:97`) and `from_row` (`tasks.rs:101`) indices.
- New fns (in `control.rs`):
  - `stop(conn, id)` → `status='stopped'`, `run_epoch = run_epoch + 1`.
  - `resume(conn, id)` → `status='pending'`, `next_run = NULL` (drop any backoff hold).
  - `restart(conn, id)` → `status='pending', step_position=0, iteration=0, auto_retries=0,
    attempt = attempt + 1, result=NULL, next_run=NULL, run_epoch = run_epoch + 1`.
  - `note_retry(conn, id, next_run: &str)` → `status='pending', auto_retries = auto_retries + 1,
    next_run = ?` (keeps `step_position` + the run chain).
  - `bump_epoch(conn, id) -> rusqlite::Result<i64>` → `UPDATE … SET run_epoch = run_epoch + 1 …
    RETURNING run_epoch` (read the new value back with `query_row`).
  - Extend `cancel` to also bump `run_epoch` (so the guard drops a cancelled task's late result).

**`task_runs.rs`:** `NewRun` + `TaskRun` gain `attempt`; `append` writes it; add
`list_for_task_attempt(conn, task_id, attempt)` (same query + `AND attempt = ?`). `list_for_task`
stays (detail view).

**Tests:** `stop_parks_and_bumps_epoch`; `resume_requeues_and_clears_next_run`;
`restart_resets_step_iteration_retries_and_bumps_attempt`; `note_retry_increments_and_holds_via_next_run`;
`stopped_is_invisible_to_next_ready` (set Stopped → `next_ready` returns None);
`migrate_adds_control_columns_to_an_old_tasks_table` (downgrade-then-migrate, like
`migrate_upgrades_an_old_shape_db_in_place` at `db/mod.rs:244`);
`list_for_task_attempt_scopes_to_one_attempt`.

**Done when:** gate green; no caller of the new fns yet.

---

## T2 — the run-epoch guard (drop stale results; harden cancel)

**Goal:** an in-flight worker's result is discarded once its dispatch is superseded.

**Files:** `src/bridge/mod.rs`, `src/bridge/scheduler.rs`.

- `Command::TaskFinished` (`mod.rs:236`) gains `epoch: i64`; update the dispatch arm
  (`mod.rs:689`) and the `finished(…)` signature.
- In `scheduler.rs`, a single launch helper stamps the epoch every spawn:

  ```rust
  fn dispatch(conn: &Connection, id: &str) -> i64 { tasks::bump_epoch(conn, id).unwrap_or(0) }
  ```

  Call it at each spawn site — `start` (`:66`), `advance_to` (`:572`), the revise re-launch
  (`:540`), and `launch_validator` (`:126`) — and thread the returned epoch through `RunMeta`
  (`:168`) → `spawn_run` (`:177`) → the `Command::TaskFinished { epoch, … }` it posts (`:199`).
- `finished()` (`:424`) opens with the guard:

  ```rust
  let Ok(Some(task)) = tasks::get(conn, id) else { return };
  if task.run_epoch != epoch { return; }   // superseded by stop/cancel/restart/re-dispatch
  ```

**Tests:** `finished` is hard to unit-test (needs `SharedServerState` + worker), so add a focused
DB-level test in `db/tasks`: `bump_epoch_returns_monotonic_values`, and assert the guard's premise
(`stop` then a fetch shows a higher `run_epoch` than a value captured before). The behavioural
check ("a stopped task's result is dropped") is covered by the manual e2e in T3.

**Done when:** gate green; cancelling a task no longer races its worker (verify by reading
`finished`'s new guard).

---

## T3 — Stop / Start / Restart commands + buttons

**Goal:** the user can drive the verbs from the board and the detail view.

**Files:** `src/bridge/mod.rs`, `src/bridge/tasks.rs`, `src/state.rs`,
`src/ui/workspace_tasks.rs`, `src/ui/task_detail.rs`.

- Commands `StopTask{id}`, `StartTask{id}`, `RestartTask{id}` (`mod.rs` enum + `handle` arms →
  `tasks::stop/resume/restart`).
- `bridge/tasks.rs`: `stop`/`resume`/`restart` wrappers — call the DB fn, then re-project the lane
  (`load`), refresh the open detail (`refresh_detail_if_open`), and the dashboard
  (`project_dashboard`). Guard `update()` (`bridge/tasks.rs:301`): refuse when the task status is
  `running` → `surface("stop the task before editing it")`.
- `state.rs`: `TaskView` (`:236`) gains `attempt`, `auto_retries`, `max_retries`, `next_retry:
  Option<String>`. `TaskDetail` (`:390`) gains the same status fields it needs for the action bar.
- UI buttons by status (`workspace_tasks.rs` card `:304`; `task_detail.rs` `body` `:22`):

  | status | controls |
  |---|---|
  | running | **Stop**, Cancel — *not* Edit |
  | pending | Edit, **Stop**, Cancel |
  | stopped | **Start**, **Restart**, Edit, Cancel |
  | done/failed/rejected/cancelled | **Restart** |

  Extend the terminal-string list at `workspace_tasks.rs:310` and `task_detail.rs:27` to treat
  `stopped` as non-terminal-but-not-running, and add `"stopped"` colouring in `status_badge`
  (`:463`). New handlers post the new commands (copy the `cancel_task` handler shape, `:508`).

**Tests:** bridge handlers run against `SharedServerState`; follow existing bridge test patterns if
present, else cover the logic in `db/tasks` (already done in T1) and verify the UI manually.
Add `update_refuses_a_running_task` (DB/bridge-level: status running → `update` is a no-op + error).

**Done when:** gate green; manual e2e — queue a task, Stop it (lane frees, card shows Stopped),
Start (resumes), Restart from a terminal card (fresh run).

---

## T4 — auto-retry on fail (bounded, backoff)

**Goal:** an execution failure retries at the same step instead of dying.

**Files:** `src/bridge/scheduler.rs`, `src/db/settings.rs` usage.

- Pure decision fn (unit-tested like `next_action` at `scheduler.rs:388`):

  ```rust
  #[derive(Debug, PartialEq, Eq)]
  enum FailNext { Retry { backoff_secs: i64 }, Fail, ResumeCron }
  fn fail_next(auto_retries: i64, max_retries: i64, recurrent: bool, base_secs: i64) -> FailNext;
  // auto_retries < max_retries → Retry { base * 2^auto_retries, capped at BACKOFF_CAP }
  // else recurrent → ResumeCron ;  else → Fail
  ```

- Route the **execution-failure** branches of `finished()` through it — the `RunRole::Stage`
  `!ok` arm (`:482`) and `RunRole::Producer` `!ok` arm (`:485`). **Do not** touch the validator
  `reject`/`on_exhaust=fail` paths (`:535`, `:553`) — those stay terminal.
- `retry_or_fail(conn, shared, &task, &output)`: read `max_retries` =
  `task.max_retries.unwrap_or_else(|| settings::get_or(conn, "task.max_retries", "2").parse())`
  and `base` from `"task.retry_base_secs"` (default `30`). Apply `fail_next`:
  - `Retry` → `tasks::note_retry(conn, id, &to_rfc3339_millis(now + backoff))`; project lane +
    detail; activity ("retry N/M, next HH:MM"). The next tick re-launches `launch_current` at the
    same step.
  - `ResumeCron` → like `reschedule_at`'s success path: compute next cron slot, `tasks::reschedule`.
  - `Fail` → `terminal_finish(Failed)` (existing).
- Reuse `crate::schedule::{to_rfc3339_millis, …}` and `now_unix` (`scheduler.rs:411`).

**Tests:** `fail_next_retries_then_resumes_or_fails` (table: under budget → Retry with doubling
backoff; at budget + recurrent → ResumeCron; at budget + one-shot → Fail);
`fail_next_caps_backoff`. A regression note in the test: a failed run is `ok=false`, and
`step_context` already filters `run.ok` (`:253`), so the retry's context excludes it.

**Done when:** gate green; manual e2e with a deliberately-failing spec (e.g. an unreachable
endpoint) — the task retries with growing backoff, then lands Failed after the budget.

---

## T5 — retry config surface

**Goal:** the global defaults are editable and a per-task override exists.

**Files:** `src/ui/prompts.rs` (the Settings/defaults section), `src/bridge/prompts.rs`,
`src/ui/workspace_tasks.rs` (edit drawer), `src/bridge/tasks.rs`.

- Add `task.max_retries` / `task.retry_base_secs` to the editable defaults (the same machinery as
  the role preambles, `bridge/mod.rs:1054`, `LoadSettings`/`SetRolePrompt`) — or a sibling
  `SetTaskSetting` command if the existing one is preamble-specific.
- Edit drawer (`edit_form`, `workspace_tasks.rs:386`): add a numeric `max_retries` input;
  `UpdateTask` (`mod.rs:213`) + `tasks::Edit` (`bridge/tasks.rs:291`) + `tasks::update`
  (`tasks.rs:324`) carry `max_retries: Option<i64>`.
- The card shows the retry indicator from `TaskView.next_retry` / `auto_retries`.

**Tests:** `update_persists_max_retries`; `settings_default_max_retries_is_2`.

**Done when:** gate green; changing the default changes retry behaviour for new failures.

---

## T6 — attempt isolation (clean Restart context)

**Goal:** a Restart doesn't inherit the prior attempt's outputs as context; history is preserved.

**Files:** `src/bridge/scheduler.rs`, `src/bridge/tasks.rs`, `src/ui/task_detail.rs`.

- The two scheduler context reads become attempt-scoped: `launch_current`'s
  `task_runs::list_for_task` (`:111`) → `list_for_task_attempt(conn, &task.id, task.attempt)`, and
  `last_producer_output` (`:296`) likewise (thread `attempt` in).
- Every `task_runs::append` in `finished`/`human_verdict` stamps `task.attempt`.
- Detail view (`build_detail`, `bridge/tasks.rs:79`) still loads all runs; group `RunView`s by
  `attempt` with a divider label ("— attempt 2 —") in `task_detail.rs`'s run-chain section
  (`:53`). `RunView` (`state.rs:374`) gains `attempt`.

**Tests:** `step_context_ignores_prior_attempt_runs` (append a run at attempt 0, bump attempt,
assert the scheduler context call returns nothing); detail grouping is visual (manual).

**Done when:** gate green; Restart a multi-step task and confirm the new run's context starts clean
while the old runs remain visible under their attempt header.

---

## T7 — "Ask owner to revise the spec"

**Goal:** delegate a spec change to the workspace owner via a short planning turn.

**Files:** `src/tools/planning.rs` (or a new `src/tools/revise.rs`), `src/tools/mod.rs`,
`src/bridge/orchestrate.rs`, `src/bridge/mod.rs`, `src/ui/workspace_tasks.rs`.

- New `ReviseToolbox { conn: Mutex<Connection>, workspace, task_id }` with one tool
  `revise_task(spec?, title?, schedule?)` → validates the task is in the workspace and **not
  `running`**, then `tasks::update(…)` (reusing `tasks.rs:324`). Mirror `PlanningToolbox`'s
  connection/threading shape (`planning.rs:35`).
- `orchestrate::ask_revise(conn, task_id, instruction, origin_chat)` — mirror `ask`
  (`orchestrate.rs:18`): resolve the task's workspace + owner, seed the planning prompt with the
  task's **current title + spec** and the user's instruction, run on a worker thread with the
  `ReviseToolbox`, post `Command::AskPlanned`-style completion (reuse `AskPlanned`/`planned`).
- `Command::AskRevise { task_id, instruction, origin_chat }` + `handle` arm.
- UI: a small "Ask owner to revise" disclosure on a **stopped** card (a textarea → `AskRevise`,
  copying the `request_changes` form shape at `task_detail.rs:341`).

**Tests:** `revise_task_updates_spec_within_workspace`; `revise_task_refuses_a_running_task`;
`revise_task_refuses_a_task_from_another_workspace` (mirror `cancel_task_only_within_the_workspace`,
`planning.rs:362`).

**Done when:** gate green; manual e2e — stop a task, "Ask owner to revise" with an instruction,
confirm the spec is rewritten and a Restart runs the new spec.

---

## T8 — Phase-2 cooperative hard-stop (optional follow-up)

**Goal:** Stop halts the worker (bounds spend + further file writes), not just its result.

**Files:** `src/agent/client.rs`, `src/agent/mod.rs`, `src/bridge/mod.rs`,
`src/bridge/scheduler.rs`.

- `run_turn_streamed` (`client.rs:43`) takes `cancel: &std::sync::atomic::AtomicBool`; check it at
  the top of the round loop (`client.rs:66`) → break (skip further model/tool calls). `run_turn`
  (blocking, used by chat/planning/summarize) passes a never-set flag, or gains the same optional
  param — keep the change minimal and update all callers.
- The bridge keeps a registry `HashMap<task_id, Arc<AtomicBool>>`; `dispatch` (T2) creates the
  flag and hands the `Arc` to the worker; `StopTask`/`CancelTask` set it.

**Tests:** `run_turn_streamed_stops_when_cancelled` (mock/echo round + a pre-set flag → returns
without a second round).

**Done when:** gate green; Stop on a long-running task ends the worker within one round.

---

## Sequencing & risk

- **T1→T2→T3** is the minimum viable slice (stop/start/restart + the guard) and can ship alone.
- **T4** (auto-retry) is independent of T3 and can land in parallel after T1.
- **T5/T6/T7** layer on; **T8** is a clean, isolated follow-up.
- Biggest risk is the epoch plumbing in T2 (every spawn site must stamp it) — the single
  `dispatch` helper is the mitigation; grep for `spawn_run(` to confirm all sites are covered.
