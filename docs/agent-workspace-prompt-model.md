# Agent ↔ workspace ↔ prompt model

> A design roadmap for how agents, workspaces, and system prompts relate. Supersedes the
> implicit model shipped in Step 2 (where each agent was glued 1:1 to a workspace and the
> system prompt *was* that workspace's `AGENT.md`).

## Context

**Today.** An agent group owns a workspace 1:1 (`agent_groups.folder = id`, `NOT NULL
UNIQUE`), and its system prompt *is* `<workspace>/AGENT.md` (composed as a hardcoded base +
`AGENT.md`). That fuses three separate concerns onto one directory: the agent's **identity**
(its persona), the **project** it works on (its files), and the **prompt store**.

**The goal** (operator's mental model): an agent is a **persistent worker**; a user holds
**many conversations** with one agent; **multiple agents can share one workspace**; and the
**prompt stays editable by the agent**. The 1:1 fusion can't express "two distinct agents
collaborating on one shared project," so the workspace must become a first-class, shareable
resource and the prompt must split into the parts that are per-agent vs per-project.

## The model

The system prompt is **layered**, and each layer has a different owner:

```
system prompt  =  base prompt          (global, hardcoded)
               +  per-agent prompt      ← identity: "who am I"     — operator-owned
               +  workspace AGENT.md     ← context: "what we build" — shared, agent-editable
```

- **Per-agent prompt** is the agent's identity/persona. Distinct per agent, so two agents on
  the same workspace stay distinct. **Operator-owned** (set in the agent drawer); agents do
  not rewrite their own persona.
- **Workspace `AGENT.md`** is the project brief. **Shared** by every agent on that workspace
  and **editable by the agents themselves** (Files view + their `write`/`edit` tools) — this
  is where "the prompt stays editable by the agent" lives.

Relations:

```
workspace ──1:N── agent_group        (N agents share one workspace; an agent has 0 or 1)
agent_group ──1:N── chat             (many conversations per agent; shared workspace = shared files)
agent_group ──1:1── per-agent prompt
workspace ──1:1── AGENT.md (project brief)
```

Why it's coherent: **identity is per-agent, context is per-workspace.** A `bash rm -rf .` in
a shared project can now wipe the project brief but never an agent's identity.

## Schema (additive only — `agent_groups.folder` left untouched for rust-claw compat)

```sql
-- The agent's identity prompt (operator-owned).
CREATE TABLE IF NOT EXISTS agent_prompts (
  agent_group_id TEXT PRIMARY KEY REFERENCES agent_groups(id),
  body           TEXT NOT NULL,
  updated_at     TEXT NOT NULL
);

-- First-class, shareable workspaces.
CREATE TABLE IF NOT EXISTS workspaces (
  id         TEXT PRIMARY KEY,   -- dir = <data>/workspaces/<id>
  name       TEXT NOT NULL,
  created_at TEXT NOT NULL
);

-- Which workspace an agent works in (N agents -> 1 workspace; an agent may have none).
CREATE TABLE IF NOT EXISTS agent_workspaces (
  agent_group_id TEXT PRIMARY KEY REFERENCES agent_groups(id),
  workspace_id   TEXT NOT NULL REFERENCES workspaces(id)
);
```

The legacy `agent_groups.folder` / `tool_profile` columns stay (rust-claw reads them) but
claw-rwire stops resolving through them.

## Resolution

- **Workspace:** `agent_workspaces` → `workspaces` → `dir_for(<data>, workspace_id)`. No row
  ⇒ the agent has no workspace (pure chat agent).
- **System prompt:** `compose(base, per_agent_prompt, read_agent_md(workspace))` — each layer
  included only when non-empty, joined by blank lines, in identity-then-context order.
- **Coder tools:** offered only when the agent (a) has a workspace and (b) has the tool
  enabled in `agent_tools`. The coder toolbox is jailed to the resolved workspace dir.
- The Files view is now keyed by **workspace** (not agent): browsing/editing a workspace and
  its `AGENT.md` affects every agent sharing it.

## UI

- **Agent drawer** — the existing "System prompt" textarea now writes the **per-agent prompt**
  (`agent_prompts`), labelled as the agent's identity. Add a **workspace picker** (assign the
  agent to an existing workspace, or none).
- **Files / workspace view** — `AGENT.md` is presented as the **workspace's project brief**
  (relabelled from a generic file), editable here and by the agent's tools. The agent
  selector becomes (or is backed by) a **workspace** selector.
- **Workspace management** — create/rename workspaces and see which agents share each
  (a small System Map surface, or folded into the Files view).

## Migration / compatibility

- On boot, for each existing agent with a scaffolded `<data>/workspaces/<folder>/AGENT.md`:
  create a `workspaces` row (`id = folder`, `name = agent name`), map the agent to it
  (`agent_workspaces`), and keep that `AGENT.md` as the workspace brief. Seed `agent_prompts`
  empty (so the prompt falls back to base + brief until the operator sets an identity).
- rust-claw sharing a `central.db` keeps working: it still reads `folder` / `tool_profile`;
  the additive tables are claw-rwire-only.

## Build order

| # | Step | Notes | Status |
|---|------|-------|--------|
| 1 | DB: `agent_prompts`, `workspaces`, `agent_workspaces` + queries | additive; `src/db/` | ✅ done |
| 2 | Resolution: 3-layer prompt compose + workspace-via-mapping | `db/resolution.rs`, `workspace` | ✅ done |
| 3 | Run wiring: resolve workspace, gate coder tools on workspace presence | `bridge` | ✅ done |
| 4 | UI: drawer per-agent prompt + workspace picker; Files keyed by workspace | `ui/` | ✅ done |
| 5 | Workspace management (create/rename/assign) | System Map or Files | ⏳ partial |
| 6 | Boot migration of existing agents → workspaces | `bridge::boot` | ✅ done |

### Implementation notes (what shipped)

- The drawer picker covers **assign to a shared workspace** and **mint the agent its own**
  ("— own workspace —", named after the agent). A new agent always gets a workspace (its own
  or a shared one); the DB + resolution *support* a workspace-less pure-chat agent (coder
  tools gated off, no brief injected), but the drawer doesn't yet expose "no workspace".
- **Step 5 is partial.** The dedicated management surface — **rename** a workspace, create a
  *named* standalone workspace, and see *which agents share* each — is not built yet. The
  `workspaces::{rename, get, unassign}` helpers were trimmed (no `#[allow(dead_code)]`) and
  should be re-added with that UI.
- Migration seeds `agent_prompts` empty: an existing agent's old `AGENT.md` becomes its
  workspace's project brief, and its identity prompt starts blank (falls back to base + brief
  until the operator sets one in the drawer).

## Open decisions

- **Workspace handle:** explicit **named workspaces** an operator creates and assigns
  *(recommended — matches "first-class resource")* vs. a bare **id you type/share** between
  agents (lighter, but no listing/naming). Leaning named.
- **Agent self-editing its own persona** via an `update_instructions` tool — deferred; the
  workspace `AGENT.md` already gives agents an editable surface.

## Out of scope (later)

- Per-chat / per-session workspaces (the Step 3 session-model pivot) — this model is
  per-agent-shared, deliberately.
- Agent-to-agent delegation (Step 4) — but a shared workspace is a natural substrate for it.
