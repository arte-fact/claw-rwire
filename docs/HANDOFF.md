# Handoff — workspace re-architecture + task board

State as of commit `9f7e06e`. Working tree clean. All checks green:
`cargo fmt --check` · `cargo clippy --all-targets -- -D warnings` · `cargo test` (**113 passing**).
**Phase C (task board) is complete** — A, B, C all done; only Phase D (rolling context) remains.

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
| `7580798` | **C-2** | scheduler scan (`next_ready`/`running_in`/`finish`) + `resolution::resolve_for_agent`; `bridge/scheduler.rs` (ticker + executor); `SchedulerTick`/`TaskFinished`; one running task per workspace, parallel across; result stored on the task + Activity |
| `7260be4` | **C-3a** | delegation: concierge `ConciergeToolbox::ask` → `Command::Ask` → `bridge/orchestrate.rs` owner **planning turn** with `PlanningToolbox` (`create_task`/`list_tasks`/`cancel_task`, member enum, schedule+blockers); `AskPlanned` re-projects lane; `tasks::add_blocker` |
| `b9bc86f` | **C-3b** | concierge **notify turn**: `origin_chat` threaded to `tasks.origin_chat_id`; `scheduler::finished` → `notify::completion` (short concierge turn) → `Command::Notified` appends to the chat (no banner/activity) |
| `b45c43c` | **C-3c rec** | recurrence: `src/schedule.rs` (dep-free 5-field cron, `next_after`, skip-to-next-slot); `next_ready` gates on `next_run`; recurrent done → `tasks::reschedule` |
| `3720c2a` | **C-3c ui** | schedule input in the create form (cron-validated); cards show `⟳ <cron> · next <ts>` + result snippet; `TaskView` enriched |
| `af86fb5` | **C-3d** | global dashboard all-lanes view (`App.dashboard_lanes`/`LaneSummary`, `project_dashboard`, `LoadDashboard` + per-tick refresh) |
| `8f6c019` | refactor | Tasks UI extracted to `ui/workspace_tasks.rs` (workspace.rs 621→448) |
| `9f7e06e` | **C-3c drawer** | per-card edit drawer (retitle/spec/reassign/schedule/blockers/cancel); `db::tasks::update`/`set_blockers`/`blockers_of`; `tasks::Edit` + `Command::UpdateTask`; `App.editing_task` |

## Next: Phase D — rolling context (per-agent memory)

Phase C is done. The remaining roadmap item is **Phase D**: a per-agent **rolling summary**
store (additive table) + summarize-on-threshold, injected into `run_turn`'s system context for
the concierge + each workspace owner — so the long-lived concierge chat and recurring owner
planning don't grow unbounded. See the roadmap's Phase D section. (Best validated with a live
`CLAW_ENDPOINT`.)

### Live-model validation still outstanding
The whole delegation loop (concierge `ask` → owner planning → task runs → notify) and recurrence
*reschedule-on-done* are wired + unit-tested, but the stub endpoint only exercises the failure
path. With a working `CLAW_ENDPOINT`/`CLAW_MODEL`, smoke-test: chat a multi-step request → the
concierge calls `ask` → cards appear in the owner's lane → they run → the concierge notifies in
chat; and a recurrent task that succeeds gets a future `next_run` and returns to `pending`.

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
  `-D warnings`. Build the consumer in the same increment, or trim to what's used. This also
  rules out `#[allow(clippy::too_many_arguments)]` — when a fn crosses ~7 args (e.g. the task
  edit path), bundle them into a struct (`bridge::tasks::Edit`) instead of silencing the lint.
- **rwire forms can't read a multi-select** (`FormData` keeps last value). Use **immediate-apply**
  controls (toggle-chips firing a command each) — see the Team tab.
- **On a `Submit` event, a form's `.data(...)` attribute isn't exposed via `ctx.data()`** —
  only `ctx.field()` (FormData) is reliable. To pass a row id through a form submit (e.g. the
  task edit drawer), put it in a hidden `<input name="id">` and read `ctx.field("id")`, not a
  data-attribute on the `<form>`. (`ctx.data()` *does* work for click handlers on buttons.)
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
