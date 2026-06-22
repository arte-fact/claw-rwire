# Handoff — workspace re-architecture, task board, agents

State as of commit `d6a8989`. Working tree clean. All checks green:
`cargo fmt --check` · `cargo clippy --all-targets -- -D warnings` · `cargo test` (**122 passing**).
**The roadmap (Phases A–D) is complete.** What remains is live-model validation, a couple of
named follow-ups, and the explicitly out-of-scope list.

## The arc

A multi-session effort to reorganize claw-rwire around **workspaces**, add a **task board**,
make **agents role-aware**, and give them **rolling memory**. Design lives in five docs — read
these first:

- `docs/agent-workspace-prompt-model.md` — agent identity prompt + workspace **owner** (home,
  ≤1) + **membership** (teams, M:N). See its *Revision* section.
- `docs/task-board.md` — concierge → owner planning turn → tasks; sequential per workspace,
  parallel across; blockers (a chain = a pipeline); concierge notify turn.
- `docs/ui-information-architecture.md` — global concierge chat at the root + workspace nav
  tree (Files/Tasks/Team) + global/workspace dashboards. Per-workspace chat deferred.
- `docs/implementation-roadmap.md` — **the living checklist** (phases A–D, all ticked, +
  decisions + out-of-scope). The source of truth for what's done / next.

## How a run is composed (the mental model)

Every model call goes through `bridge::run_system_prompt`, which layers a **4-part system
prompt**:

1. **Role preamble** (`agent::CONCIERGE_PROMPT` / `EXECUTOR_PROMPT` / `OWNER_PLANNER_PROMPT`) —
   what this *run* is for. Chat delegates + never fabricates; a task executor uses its file
   tools for real; an owner plans into `create_task` cards.
2. **Identity persona** (`agent_prompts`, per-agent) — who this agent is. Auto-seeded
   (generalist for the concierge/owner, specialist for new agents); editable in the drawer.
3. **Workspace brief** (`AGENT.md`, shared) — what this workspace is for + the folder
   conventions (`projects/` / `docs/` / `skills/`, created on scaffold).
4. **Working memory** (`agent_memory`, Phase D) — a rolling summary of earlier context.

Three run sites: `dispatch_native` (chat/concierge), `scheduler::launch` (task executor),
`orchestrate::ask` (owner planning). Each resolves a `ResolvedInference` (now carrying
`agent_id`) and injects all four layers.

## Done (committed)

| Commit | Phase | What |
|---|---|---|
| `cdc46bb` | **A** | workspace `owner_agent_id` (partial-unique) + `agent_workspaces` M:N; `db::migrate` upgrades old-shape DBs; owner/membership on the Team tab |
| `159198d` | **B-1** | routes `/dashboard`, `/w/:wid[/files|/tasks|/team]`; `workspace::shell`; dashboards; sidebar Workspaces; `App.active_workspace` |
| `191cd34` | **B-2** | editable **Team tab** (immediate-apply); dashboard "New workspace"; System Map shrunk to Channels/Agents/Endpoints |
| `9acc1ab` | **B-3** | "Chat" tree label; inline `AGENT.md` brief editor |
| `ee7c7c6` | **C-1** | `tasks` + `task_blockers` schema; `db::tasks`; manual **Tasks tab** |
| `7580798` | **C-2** | `bridge/scheduler.rs` ticker + executor; `next_ready`/`running_in`/`finish`; one running task/workspace, parallel across; `resolve_for_agent` |
| `7260be4` | **C-3a** | delegation: `ConciergeToolbox::ask` → `Command::Ask` → `bridge/orchestrate.rs` owner planning turn with `PlanningToolbox` |
| `b9bc86f` | **C-3b** | concierge **notify turn**: `origin_chat` → `tasks.origin_chat_id`; `notify::completion` → `Command::Notified` |
| `b45c43c` | **C-3c** | recurrence: `src/schedule.rs` (dep-free cron, skip-to-next-slot); `next_ready` gates on `next_run`; `tasks::reschedule` |
| `3720c2a` | **C-3c** | schedule input in create form (cron-validated); richer cards |
| `af86fb5` | **C-3d** | global dashboard all-lanes view (`App.dashboard_lanes`/`project_dashboard`) |
| `8f6c019` | refactor | Tasks UI → `ui/workspace_tasks.rs` |
| `9f7e06e` | **C-3c** | per-card **edit drawer**; `tasks::Edit` + `Command::UpdateTask`; `App.editing_task` |
| `283747e` | hardening | **role-aware prompts** + executor defaults to **workspace tools** (fixes tool-less owner) |
| `27c2757` | hardening | **tool-call parser**: recover leaked `…invoke name=…`/`<tool_call>` calls; replay tool results as `[result of …]` so the model stops imitating `[tool:…]` |
| `ebb3bec` | hardening | **defaults**: workspace `projects/`/`docs/`/`skills/` + conventions brief; auto-seeded personas |
| `6b2069e` | **D** | per-agent **rolling memory**: `agent_memory` store, 4th prompt layer, windowed transcript (`model_turns_after`), summarize-on-threshold worker |
| `d6a8989` | **D** | **token-aware** fold: `usage.prompt_tokens` + model `context_length` from `/models`; fold at 60% of context, turn-count fallback; context cached per (endpoint,model) |

## Why the hardening commits exist (read if touching agents)

A diagnostic of the user's `claw-data/` test session found the delegation loop failing three
ways — all now fixed, but the root causes are worth knowing:
1. **The owner had no workspace tools** → owner-run tasks replied "I don't have filesystem
   access." Fix: executors default to workspace tools (`283747e`).
2. **`deepseek-v4-flash` serialises tool calls as text** (`…invoke name="x"…` DSML, or
   `[tool:ask] …` prose) instead of the structured `tool_calls` field → half the delegations
   silently no-op'd. Fix: the recovery parser + neutral replay (`27c2757`). **This is a model
   trait — the parser is a safety net, not a substitute for a tool-use-native model.**
3. **The concierge fabricated task outcomes** and did workspace work over `fetch`. Fix: the
   `CONCIERGE_PROMPT` role preamble (`283747e`).

## What's left

- **Live-model validation (the big one).** Everything model-driven is wired + unit-tested but
  the stub endpoint only hits the *failure* path. With a real `CLAW_ENDPOINT`/`CLAW_MODEL`,
  smoke-test end to end: chat a multi-step request → concierge `ask` → cards in the owner's
  lane → they run (writing real files) → concierge notifies in chat; a recurrent task → future
  `next_run` → back to `pending`; a long chat → memory summary appears + the tail windows.
  Prefer a **tool-use-native model** over deepseek-v4-flash.
- **Owner/executor memory (follow-up, roadmap-noted).** Phase D *injects* memory into every run
  but only *builds* it for the concierge (the one long thread). Owners/executors are
  single-turn, so their memory stays empty. Build it by folding task outcomes into a
  per-workspace worklog so the team remembers prior work (also fixes the "already exists / redid
  it" behaviour).
- **Out of scope** (roadmap, not "todo"): within-workspace parallelism, cross-workspace
  pipelines/blockers, cross-workspace search, SMS channel.

## Gotchas (important — these cost real time)

- **`./claw-data` is the default data dir** (`main.rs`, and `.env` sets it explicitly with
  `CLAW_BIND=0.0.0.0:8080`). Any `cargo build` + boot **without** `CLAW_DATA_DIR` set — or the
  user's own live instance picking up a rebuilt binary — writes to the **real** data. Always run
  test boots with `CLAW_DATA_DIR=/tmp/...`. (Boot seeds are additive/idempotent, so the blast
  radius is small, but don't assume `claw-data` is untouched.)
- **Stale release binary.** `cargo build --release` can report `Finished` *without recompiling*
  after edits. `touch src/main.rs` first and verify `stat -c %y target/release/claw-rwire` is
  newer than the source.
- **rwire `#[renderer]` binds ONE state** and infers field-deps from the **body** — read fields
  **inline**, not inside a helper fn, or it won't re-render. `#[renderer(always)]` is a no-op.
- **`on_route` must not write `App`** — route-hook mutations don't broadcast; project through a
  `Command` (`SetActiveWorkspace`, `LoadTasks`, `LoadDashboard`, …).
- **On a `Submit` event, a form's `.data(...)` isn't exposed via `ctx.data()`** — only
  `ctx.field()` (FormData). Pass row ids through a hidden `<input name="id">` (see the task edit
  drawer). `ctx.data()` *does* work for button click handlers.
- **No `#[allow]` / dead_code** (project rule). A DB fn with no consumer fails `-D warnings` —
  build the consumer in the same increment. Also rules out `#[allow(clippy::too_many_arguments)]`:
  bundle into a struct (e.g. `bridge::tasks::Edit`) past ~7 args.
- **rwire forms can't read a multi-select** (`FormData` keeps last value) — use immediate-apply
  controls (toggle-chips firing a command each), as the Team tab does.
- **Schema migrations**: new *tables* are safe via `CREATE TABLE IF NOT EXISTS` (the schema is
  applied idempotently at open). New *columns* / PK changes on existing tables need `db::migrate`
  (idempotent ALTER / table-rebuild). Task/memory tables were added table-only.
- **`WorkspaceError`/`rusqlite::Error` aren't `PartialEq`** — tests use `matches!`.
- **Blocking work goes on a worker thread**, never the bridge thread (it owns the DB conn +
  command loop). `run_turn` and the `/models` context fetch (`warm_context_cache`) both run on
  spawned workers that post a `Command` back.

## Run / e2e harness (what worked)

```bash
touch src/main.rs && cargo build --release            # avoid the stale-binary trap
D=/tmp/claw-ui/X; rm -rf $D; mkdir -p $D               # ALWAYS a throwaway data dir
CLAW_BIND=127.0.0.1:PORT CLAW_DATA_DIR=$D CLAW_AUTH_USER=claw CLAW_AUTH_TOKEN=shot \
  CLAW_ENDPOINT=http://127.0.0.1:9999/v1 CLAW_MODEL=test-model CLAW_SCHEDULER_SECS=0 \
  LD_LIBRARY_PATH="$HOME/.local/lib" setsid ./target/release/claw-rwire >$D.log 2>&1 &
```
- **Login is a form**, not URL basic-auth: `fill input[name=username]=claw`,
  `input[name=password]=shot`, click `button:has-text("Sign in")`, then navigate.
- Drive UI with Playwright (`LD_LIBRARY_PATH="$HOME/.local/lib" python3 script.py PORT`);
  inspect with `sqlite3 $D/central.db "…"`.
- `CLAW_SCHEDULER_SECS=0` disables the task scheduler; set `2` to watch tasks run.
- The throwaway endpoint (`:9999`, nothing listening) exercises the **failure** path cleanly —
  tasks → `failed`, notify/summarize workers reach `run_turn` then log the connection error.
- Default seed: one agent `claw` (`ag-default`, generalist persona) owning workspace `claw`.
