# UI information architecture — global chat + workspace tree + dashboards

> One **concierge** chat at the root (the single agent you talk to), and **workspaces** as
> work containers (`Files / Tasks / Team`) in a nav tree. A **global dashboard** shows all
> workspaces at a glance; each workspace has its own **dashboard**. Chat is **global only**
> for v1 — per-workspace chat is deferred. Builds on the
> [owner + membership model](agent-workspace-prompt-model.md#revision-planned-with-the-task-board-workspace-owner--multi-workspace-membership)
> and the [task board](task-board.md).

## The agents — three tiers

```
user ── global chat ──► CONCIERGE          the one chat owner; converses (rolling context),
                           │                routes work, relays results to you
                           │ tool: ask(workspace, request)
                           ▼
                     WORKSPACE OWNER        a workspace's domain lead; runs a short PLANNING
                           │                turn (request → a plan + the tasks it creates)
                           ▼
                     MEMBERS                execute the workspace's task lane
        results flow back: task done → owner → concierge → you, in the global chat
```

The **concierge** is the only user-facing chat agent (today's `web_agent`, re-roled). A
**workspace owner** is *not* a chat surface — it receives a delegated request, decomposes it
into tasks, and returns a plan. That keeps one assistant in front of the user with the team
behind it.

## The shell

```
┌ sidebar = nav tree ───────────────────┐
│ 💬 Chat               ← GLOBAL concierge (one thread list, rolling context)
│ ⌂ Dashboard           ← GLOBAL overview (all workspace lanes)
│ ──────────────────────────────────────  │
│ ▾ website          ← name → WORKSPACE dashboard
│    ⌗ Files                              │
│    ☑ Tasks (2)                          │
│    ⚲ Team   owner claw · ◓◓◓            │
│ ▸ research          (collapsed)         │
│ ▸ personal                              │
│ + New workspace                         │
│ ──────────────────────────────────────  │
│ ⚙ System   ◷ Activity   ▤ Logs   ⎋ Out  │  GLOBAL footer (app-level infra)
└──────────────────────────────────────────┘   main area = the selected node's view
```

You talk to the concierge at the root; workspaces below are pure work containers.

## Global chat (the concierge) — the landing

The app opens here. One thread list with the concierge, **rolling context** (a summarized,
persistent memory of the conversation). The concierge answers directly, or `ask(...)`s a
workspace owner to spin up tasks — and relays the owner's plan + later the task results back
into this thread.

## Global dashboard

The cross-workspace overview — the only place the parallelism is visible at once:

- one card per workspace: **task-lane status** (running task / queued / idle), owner + team;
- jump into any workspace (→ its dashboard) or any running task.

## Workspace dashboard (a workspace's home)

Reached by clicking the workspace name in the tree:

- header: **name, owner, team** (member avatars), quick action (New task);
- the **task queue** (running + what's next, blockers);
- recent **requests** the concierge dispatched here + their outcomes;
- the **brief** — the workspace's `AGENT.md`, shown and editable here (it *is* `AGENT.md`
  in Files; the dashboard is just the friendly editor for it).

## The three sections (per workspace)

| Section | What it is |
|---|---|
| **Files** | the workspace directory browser/editor — **scoped, no selector** |
| **Tasks** | this workspace's **sequential lane** (queue, blockers, recurrence) |
| **Team** | the **owner + members**; membership is managed here (multi-select agents) |

No per-workspace **Chats** section in v1 — conversation lives in the global concierge chat.

## Routes

```
/                      global chat (the concierge) — landing
/chat/:cid             a concierge thread
/dashboard             global dashboard (all workspace lanes)
/w/:wid                workspace dashboard (home)
/w/:wid/files          Files section
/w/:wid/tasks          Tasks section (the lane)
/w/:wid/team           Team section
/system /activity /logs  global, app-level
```

The active workspace is derived from the route; the tree highlights the open branch + leaf.

## What changes from today

- **Sidebar**: flat chat list → **Chat at the root** (global concierge) + a **workspace tree**
  (`Files / Tasks / Team`). New top-level **Dashboard**; new per-workspace **dashboard**.
- **Chat stays global** — we **keep the single front agent** (the concierge = today's
  `web_agent`, re-roled). No chat↔workspace mapping, no `web_agent` dissolution. *(This is the
  simplification vs. the earlier per-workspace-chat plan.)*
- **Files**: drop the agent/workspace selector; it's the active workspace's files.
- **Tasks**: new section = the workspace's lane; the **all-lanes parallel view lives on the
  global dashboard**.
- **Team**: new section = owner + members; this is where the agent-drawer's old single
  workspace picker moves to (now multi-select membership, + the owner).
- **System Map**: shrinks to **infra admin** — Agents (registry), Endpoints, Channels
  (SMS, later). Its **Workspaces tier folds** into the tree + "New workspace" + the Team
  section. Workspace *settings* (rename, delete, owner) live on the workspace dashboard.

## Mobile

The tree (Chat + Dashboard + workspaces + footer) collapses into the existing slide-in nav
drawer. The chat, dashboards, and sections are full-width routes, as today.

## Open / deferred

- **Per-workspace chat** — a focused thread with a workspace owner (drill-in). Purely
  additive: a workspace dashboard gains a thread; nothing here restructures. Deferred.
- **Tree scope (resolved): show all workspaces** — single-operator; per-user filtering later.
- Cross-workspace **search**; per-section badges (queued/running) — later.
