# Handoff — workspace re-architecture + task board

State as of commit `ee7c7c6`. Working tree clean. All checks green:
`cargo fmt --check` · `cargo clippy --all-targets -- -D warnings` · `cargo test` (**84 passing**).

## The arc

A multi-session effort to reorganize claw-rwire around **workspaces** and add a **task board**.
The design is settled across five docs — read these first:

- `docs/agent-workspace-prompt-model.md` — agent identity prompt + workspace **owner** (home,
  ≤1) + **membership** (teams, M:N). See its *Revision* section.
- `docs/task-board.md` — concierge → owner planning turn → tasks; sequential per workspace,
  parallel across; blockers (a chain = a pipeline); concierge notify turn.
- `docs/ui-information-architecture.md` — global concierge chat at the root + workspace nav
  tree (Files/Tasks/Team) + global/workspace dashboards. Per-workspace chat deferred.
- `docs/implementation-roadmap.md` — **the living checklist** (phases A–D, decisions). This is
  the source of truth for what's done / next. Keep it ticked.

## Done (committed)

| Commit | Phase | What |
|---|---|---|
| `cdc46bb` | **A** | workspace `owner_agent_id` (partial-unique) + `agent_workspaces` M:N; `db::migrate` upgrades old-shape DBs in place; owner/membership managed in the workspace Team tab; the agent drawer's workspace picker removed |
| `159198d` | **B-1** | routes `/dashboard`, `/w/:wid[/files|/tasks|/team]`; `workspace::shell` (header+tabs); global + workspace dashboards; sidebar Workspaces group; `App.active_workspace` (projected via `Command::SetActiveWorkspace`) |
| `191cd34` | **B-2** | editable **Team tab** (immediate-apply: rename/owner/members/delete via granular commands); dashboard "New workspace"; System Map shrunk to Channels/Agents/Endpoints (Workspaces tier deleted); concierge relabel |
| `9acc1ab` | **B-3** | "Chat" tree label; inline `AGENT.md` brief editor on Overview (`LoadBrief`/`SaveBrief`) |
| `ee7c7c6` | **C-1** | `tasks` + `task_blockers` schema; `db::tasks` (create/get/list/cancel); **Tasks tab** (create form + lane cards + cancel); `bridge::tasks`, `App.tasks` |

## Next: Phase C-2 — scheduler + executor

The Tasks tab currently queues tasks that sit `pending` (nothing runs them). C-2 makes them
execute. Plan (already designed; the queries were prototyped then trimmed from `db::tasks` —
re-add them):

1. **`db::tasks` scan functions** (re-add, with tests):
   - `next_ready(conn, wid) -> Option<Task>` — `pending`, all blockers `done`, due if
     recurrent (`next_run <= now`), ordered `priority DESC, created_at ASC`.
   - `running_in(conn, wid) -> Option<Task>` — the lane's running task (busy check).
   - `finish(conn, id, status, result)` — terminal status + result + `last_run`.
   - `blockers_done(conn, id) -> bool`, `add_blocker`, `blockers_of`.
   - `list_active(conn)` — for the dashboard lane view.
   (These existed in a prior draft of `tasks.rs`; see git history of that file if useful.)
2. **Scheduler thread + commands** in `bridge/mod.rs`:
   - A thread that sleeps ~Ns and `bridge::send(Command::SchedulerTick)` (mirror the existing
     pattern; do **not** add tokio — the thread+mpsc+`update_shared_changed` seam is enough).
   - `Command::SchedulerTick` handler: for each workspace with no `running_in`, pick
     `next_ready`, set it `running`, spawn an executor worker.
   - Executor = `agent::run_turn` on the task's agent (default = workspace owner) **in the
     workspace dir** — this is the *exact* shape of `dispatch_native`'s worker (resolve
     inference → build toolbox → `run_turn` → post a command back). Post
     `Command::TaskFinished { id, result, ok }`.
   - `TaskFinished` handler: `finish(...)`, re-project the lane, then the next tick picks up
     the lane's next task.
   - One running task per workspace; workspaces independent → parallel.

## Phase C-3 (after C-2) — orchestration + recurrence

Concierge `ask(workspace, request)` tool → owner **planning turn** with
`create_task`/`list_tasks`/`cancel_task` (a Toolbox, like `CoderToolbox`); concierge **notify
turn** posts the result into the global chat; recurrence (cron → `next_run`); blockers + edit
drawer in the Tasks UI; the dashboard all-lanes view (`list_active`).

## Gotchas learned this session (important)

- **Stale release binary.** `cargo build --release` often reports `Finished 0.10s` *without
  recompiling* after edits (cross-crate cache quirk). Always `touch src/main.rs` (or the edited
  file) before a release build, and verify `stat -c %y target/release/claw-rwire` is newer than
  the source. I lost real time to this twice — the code was right, the binary was old.
- **rwire `#[renderer]` binds ONE state** (first `&State` arg) and infers field-deps from the
  **body**. If you read a field inside a *helper fn* the renderer calls, the dep is missed and
  it won't re-render. Read the fields **inline** in the renderer (see `workspace::header`).
  `#[renderer(always)]` is a **no-op** (the macro ignores its attr).
- **`on_route` must not write `App`** — route-hook `App` mutations don't reliably broadcast.
  Project through the bridge instead (`Command::SetActiveWorkspace`, `LoadTasks`, etc.).
- **No `#[allow]` / dead_code** (project rule). A DB layer with no consumer fails
  `-D warnings`. Build the consumer in the same increment, or trim to what's used (that's why
  `db::tasks` only has create/get/list/cancel right now — C-2 re-adds the rest *with* the
  scheduler that uses them).
- **rwire forms can't read a multi-select** (`FormData` keeps last value). Use **immediate-apply**
  controls (toggle-chips firing a command each) — see the Team tab.
- **Schema migrations**: new *tables* are safe via `CREATE TABLE IF NOT EXISTS`. New *columns*
  or PK changes on existing claw-rwire-only tables need `db::migrate` (idempotent ALTER /
  table-rebuild) — see `add_workspace_owner_column` / `widen_agent_workspaces_to_m_n`.
- **`WorkspaceError`/`rusqlite::Error` aren't `PartialEq`** — tests use `matches!`, not `assert_eq!`.

## Validation note

C-2/C-3 execute via `run_turn` (a real LLM call). With the throwaway test endpoint
(`CLAW_ENDPOINT=http://localhost:9999/v1`) only the *failure* path runs (tasks → `failed`),
which is a valid test of the scheduling machinery. To validate real execution + the concierge
tools, point `CLAW_ENDPOINT`/`CLAW_MODEL` at a working provider.

## Run / e2e harness (what I used)

```bash
# fresh release binary
touch src/main.rs && cargo build --release
# launch (fresh data dir per run)
CLAW_BIND=127.0.0.1:PORT CLAW_DATA_DIR=/tmp/claw-ui/X CLAW_AUTH_USER=claw CLAW_AUTH_TOKEN=shot \
  CLAW_ENDPOINT=http://localhost:9999/v1 CLAW_MODEL=test-model \
  LD_LIBRARY_PATH="$HOME/.local/lib" setsid ./target/release/claw-rwire >/tmp/claw-ui/X.log 2>&1 &
# Playwright: login form (input #0 = user, input[type=password] = token, "Sign in")
# LD_LIBRARY_PATH="$HOME/.local/lib" python3 script.py
# inspect: sqlite3 /tmp/claw-ui/X/central.db "SELECT …"
```
Default seed: one agent `claw` (id `ag-default`) owning workspace `claw`.
