# Implementation roadmap — workspaces, task board, workspace-tree shell

> Living checklist for the workspace-centric re-architecture. Detail lives in the three
> design docs; this tracks **what's done and what's next**. Tick boxes as work lands. Every
> ticked item must pass `cargo fmt --check` · `cargo clippy --all-targets -- -D warnings` ·
> `cargo test`.
>
> Design docs: [model](agent-workspace-prompt-model.md) · [task board](task-board.md) ·
> [shell / IA](ui-information-architecture.md)
>
> **Build order is by dependency:** Phase A (data foundation) → Phase B (shell) → Phase C
> (task board). Each phase assumes the previous.

---

## ✅ Phase 0 — shipped (the base these build on)

- [x] Per-agent identity prompt (`agent_prompts`) — 3-layer system prompt
- [x] First-class workspaces (`workspaces`) + **1:1** agent assignment (`agent_workspaces`)
- [x] Workspace resolution + boot migration (folder → workspace)
- [x] System Map tiers: Channels / Agents / **Workspaces** / Endpoints (full CRUD)
- [x] Workspace tools (renamed from coder) under the agent drawer's Workspace section
- [x] Workspace deletion (guarded by assignment) + two-step confirm
- [x] Files view keyed by workspace (selector lists workspaces)
- [x] Shared button family + mobile padding

---

## ✅ Phase A — foundation: workspace owner + M:N membership

> Splits agent↔workspace into **owns (home, ≤1)** + **member-of (teams, M:N)**, and adds a
> workspace **owner**. Chat **stays global** (the concierge = today's `web_agent`, re-roled),
> so there's **no chat↔workspace mapping and no `web_agent` dissolution** — that's the
> simplification from choosing global-only chat. See the model doc's *Revision* section.

- [x] Schema: `workspaces.owner_agent_id` (partial-**UNIQUE** index) — column + ALTER migration
      + boot backfill (owner = the agent that owned the migrated folder)
- [x] Schema: `agent_workspaces` PK → `(agent_group_id, workspace_id)` (M:N) — table-rebuild
      migration (idempotent, detects the old single-column PK)
- [x] Queries: `owned_by` / `members_of` / `set_owner` / `add_member` / `remove_member`
      (`member_of` deferred to Phase C, where the executor needs it)
- [x] Resolution: a chat turn resolves the agent's **owned** workspace (`owned_by`)
- [x] Concierge role: single front agent kept (no `web_agent` dissolution). *Visible relabel*
      to "Concierge" deferred to Phase B (where Chat moves to the root)
- [x] Team management folded into the Workspaces tier: owner picker + members multi-select;
      the agent-drawer workspace picker removed (workspace tools shown unconditionally)
- [x] `cargo fmt --check` · `clippy -D warnings` · `cargo test` green (79) + e2e verified

---

## ☐ Phase B — shell: global chat + workspace tree + dashboards

> The big UI rework. See the IA doc. Chat is **global (concierge) at the root**; workspaces
> are work containers (`Files / Tasks / Team`) — **no per-workspace Chats** in v1.

**B-1 — navigation shell (done):**
- [x] `App.active_workspace` (projected via `Command::SetActiveWorkspace` from `on_route`)
      + scoped routes (`/`, `/chat/:id`, `/dashboard`, `/w/:wid[/files|/tasks|/team]`)
- [x] Sidebar **Workspaces** group (links → each workspace, active highlighted) + Dashboard
      in the footer nav; `+ New workspace` (→ System for now)
- [x] **Global dashboard** (`/dashboard`) — per-workspace cards (owner, team, idle status)
- [x] **Workspace view** (`/w/:wid…`) — header (name · owner · team) + tab row (Overview /
      Files / Tasks / Team)
- [x] Files section — scoped to the route's workspace (selector dropped)
- [x] Workspace **Overview** (team roster + brief pointer); **Team** read-only; **Tasks**
      placeholder
- [x] `fmt` · `clippy -D` · `cargo test` (79) green + e2e screenshots

**B-2 — management relocation + System Map shrink (done):**
- [x] Workspace management moved into the workspace context: an **editable Team tab**
      (rename / owner / members / delete) with **immediate-apply** controls (granular
      `CreateWorkspace`/`RenameWorkspace`/`SetWorkspaceOwner`/`AddMember`/`RemoveMember`
      commands), replacing `UpsertWorkspace` + the `WorkspaceDraft` drawer
- [x] **New workspace** create on the global dashboard; sidebar "New workspace" → `/dashboard`
- [x] System Map shrunk to infra: Channels / Agents / Endpoints; **Workspaces tier removed**
      (module + `Editing::Workspace` + `WorkspaceDraft` deleted)
- [x] Concierge relabel (System Map channel: "Concierge → …", drawer label "Concierge")
- [x] `cargo fmt --check` · `clippy -D` · `cargo test` (80) green + e2e verified

**B-3 — polish (deferred):**
- [ ] Explicit "Chat" node at the tree root; global-chat landing polish
- [ ] Workspace dashboard inline brief (`AGENT.md`) editor
- [ ] Mobile: confirm the tree in the slide-in nav drawer

---

## ☐ Phase C — task board

> Workspace-owned tasks: sequential per workspace, parallel across, blockers, recurrence.
> See the task-board doc.

- [ ] Schema: `tasks` + `task_blockers` + query module (CRUD, per-lane ready-scan)
- [ ] Scheduler thread + `Command::SchedulerTick` / `TaskFinished`; **one running task per
      workspace**, parallel across workspaces
- [ ] Executor = `run_turn` on the task's agent **in the workspace dir** (reuse the worker
      pattern)
- [ ] Tools: concierge `ask(workspace, request)` → owner **planning turn** with
      `create_task` / `list_tasks` / `cancel_task` (auto-route by member persona)
- [ ] Concierge notify turn → message in the global chat + Activity entry
- [ ] Tasks section (per-workspace lane) + edit drawer (reassign / spec / schedule / blockers
      / cancel)
- [ ] Recurrence (cron → `next_run`) + blocker-aware ordering
- [ ] Global dashboard: the all-lanes parallel view
- [ ] `cargo fmt --check` · `clippy -D warnings` · `cargo test` green + e2e

---

## ☐ Phase D — rolling context (per-agent memory)

> Each agent (concierge + each workspace owner) keeps a **summarized, persistent memory** of
> its own thread, so context survives across turns/tasks without unbounded growth. Makes the
> long-lived concierge chat and recurring owner planning viable.

- [ ] Per-agent rolling summary store (additive table) + summarize-on-threshold
- [ ] Inject the rolling summary into `run_turn`'s system context (concierge + owners)
- [ ] `cargo fmt --check` · `clippy -D warnings` · `cargo test` green

---

## Decisions (resolved)

- [x] **Tree scope** — show **all workspaces** (single-operator; filtering is a later
      multi-user concern)
- [x] **Recurrence catch-up** — **skip to the next slot** if the app was down (no catch-up
      burst on boot)
- [x] **Auto-route** — **persona enum in the `create_task` tool schema**; the model picks the
      member by its identity prompt
- [x] **Assignee scope** — **restrict to workspace members** (membership = the access
      boundary; the enum is exactly the team)

## Out of scope (later)

- Parallelism *within* a workspace (needs run isolation — subdir / worktree copy)
- Non-linear pipeline editing; cross-workspace blockers / notification
- Cross-workspace search; per-section unread/running badges polish
- SMS channel + notification (tracked in `feature-roadmap.md`)
