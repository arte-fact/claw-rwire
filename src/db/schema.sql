-- Central DB schema for claw-rwire (clean-room rewrite).
--
-- This is the collapsed final state of rust-claw's migration chain (db/migrations.rs,
-- migrations 001..008). Kept format-compatible so an existing claw central.db opens
-- unchanged; new databases are created directly at this shape.
--
-- The per-session message ledgers (messages_in/out, seq parity) live in their own
-- per-session DBs and are defined separately — this file is the central registry only.

CREATE TABLE IF NOT EXISTS endpoints (
  name        TEXT PRIMARY KEY,
  base_url    TEXT NOT NULL,
  api_key     TEXT,
  api_key_env TEXT,
  notes       TEXT,
  created_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS agent_groups (
  id             TEXT PRIMARY KEY,
  name           TEXT NOT NULL,
  folder         TEXT NOT NULL UNIQUE,
  agent_provider TEXT,
  endpoint       TEXT REFERENCES endpoints(name),
  model          TEXT,
  thinking_level TEXT,
  cli_scope      TEXT NOT NULL DEFAULT 'group',
  tool_profile   TEXT NOT NULL DEFAULT 'chat',
  created_at     TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS messaging_groups (
  id                    TEXT PRIMARY KEY,
  channel_type          TEXT NOT NULL,
  platform_id           TEXT NOT NULL,
  name                  TEXT,
  is_group              INTEGER NOT NULL DEFAULT 0,
  unknown_sender_policy TEXT NOT NULL DEFAULT 'strict',
  denied_at             TEXT,
  archived_at           TEXT,
  created_at            TEXT NOT NULL,
  UNIQUE(channel_type, platform_id)
);

CREATE TABLE IF NOT EXISTS messaging_group_agents (
  id                     TEXT PRIMARY KEY,
  messaging_group_id     TEXT NOT NULL REFERENCES messaging_groups(id),
  agent_group_id         TEXT NOT NULL REFERENCES agent_groups(id),
  engage_mode            TEXT NOT NULL DEFAULT 'mention',
  engage_pattern         TEXT,
  sender_scope           TEXT NOT NULL DEFAULT 'all',
  ignored_message_policy TEXT NOT NULL DEFAULT 'drop',
  session_mode           TEXT NOT NULL DEFAULT 'shared',
  priority               INTEGER NOT NULL DEFAULT 0,
  created_at             TEXT NOT NULL,
  UNIQUE(messaging_group_id, agent_group_id)
);

CREATE TABLE IF NOT EXISTS users (
  id           TEXT PRIMARY KEY,
  kind         TEXT NOT NULL,
  display_name TEXT,
  created_at   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS user_roles (
  user_id        TEXT NOT NULL REFERENCES users(id),
  role           TEXT NOT NULL,
  agent_group_id TEXT REFERENCES agent_groups(id),
  granted_by     TEXT REFERENCES users(id),
  granted_at     TEXT NOT NULL,
  PRIMARY KEY (user_id, role, agent_group_id)
);
-- NULLs are pairwise-distinct in SQLite unique constraints; global grants need their own index
CREATE UNIQUE INDEX IF NOT EXISTS idx_user_roles_global
  ON user_roles(user_id, role) WHERE agent_group_id IS NULL;

CREATE TABLE IF NOT EXISTS agent_group_members (
  user_id        TEXT NOT NULL REFERENCES users(id),
  agent_group_id TEXT NOT NULL REFERENCES agent_groups(id),
  added_by       TEXT REFERENCES users(id),
  added_at       TEXT NOT NULL,
  PRIMARY KEY (user_id, agent_group_id)
);

CREATE TABLE IF NOT EXISTS user_dms (
  user_id            TEXT NOT NULL REFERENCES users(id),
  channel_type       TEXT NOT NULL,
  messaging_group_id TEXT NOT NULL REFERENCES messaging_groups(id),
  resolved_at        TEXT NOT NULL,
  PRIMARY KEY (user_id, channel_type)
);

CREATE TABLE IF NOT EXISTS sessions (
  id                 TEXT PRIMARY KEY,
  agent_group_id     TEXT NOT NULL REFERENCES agent_groups(id),
  messaging_group_id TEXT REFERENCES messaging_groups(id),
  thread_id          TEXT,
  status             TEXT NOT NULL DEFAULT 'active',
  last_active        TEXT,
  created_at         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sessions_agent_group ON sessions(agent_group_id);
CREATE INDEX IF NOT EXISTS idx_sessions_lookup ON sessions(messaging_group_id, thread_id);

CREATE TABLE IF NOT EXISTS pending_questions (
  question_id    TEXT PRIMARY KEY,
  session_id     TEXT NOT NULL REFERENCES sessions(id),
  message_out_id TEXT NOT NULL,
  platform_id    TEXT,
  channel_type   TEXT,
  thread_id      TEXT,
  title          TEXT NOT NULL,
  options_json   TEXT NOT NULL,
  created_at     TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS pending_approvals (
  approval_id    TEXT PRIMARY KEY,
  session_id     TEXT NOT NULL REFERENCES sessions(id),
  agent_group_id TEXT NOT NULL REFERENCES agent_groups(id),
  command        TEXT NOT NULL,
  args_json      TEXT NOT NULL,
  platform_id    TEXT,
  channel_type   TEXT,
  thread_id      TEXT,
  summary        TEXT NOT NULL,
  created_at     TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS dropped_messages (
  id           TEXT PRIMARY KEY,
  channel_type TEXT NOT NULL,
  platform_id  TEXT NOT NULL,
  reason       TEXT NOT NULL,
  content      TEXT,
  created_at   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS web_messages (
  id                 INTEGER PRIMARY KEY AUTOINCREMENT,
  messaging_group_id TEXT NOT NULL REFERENCES messaging_groups(id),
  direction          TEXT NOT NULL,
  sender             TEXT NOT NULL,
  body               TEXT NOT NULL,
  message_out_id     TEXT,
  kind               TEXT NOT NULL DEFAULT 'chat',
  question_id        TEXT,
  options_json       TEXT,
  answer             TEXT,
  created_at         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_web_messages_group ON web_messages(messaging_group_id, id);

-- Sidecar for tool turns: a `web_messages` row with kind='tool' carries the model-facing
-- note in `body` and ordering; the structured payload for the UI card lives here. Additive
-- (claw-rwire-only) — rust-claw never reads it, so it doesn't break the shared format.
CREATE TABLE IF NOT EXISTS web_message_tools (
  web_message_id INTEGER PRIMARY KEY REFERENCES web_messages(id),
  name           TEXT NOT NULL,
  arguments      TEXT NOT NULL,
  result         TEXT NOT NULL,
  status         TEXT NOT NULL,
  created_at     TEXT NOT NULL
);

-- Per-agent tool selection: one row per (agent, tool) with an enabled flag. Additive
-- (claw-rwire-only). Absence of any row for an agent means "unconfigured" → the run falls
-- back to a default set; once the drawer saves, every catalog tool gets a row, so an
-- explicitly empty selection is distinguishable from never-configured.
CREATE TABLE IF NOT EXISTS agent_tools (
  agent_group_id TEXT NOT NULL REFERENCES agent_groups(id),
  tool_name      TEXT NOT NULL,
  enabled        INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (agent_group_id, tool_name)
);

-- The agent's identity prompt ("who am I"), operator-owned. Kept out of the workspace so
-- a shared workspace's AGENT.md stays the project brief while each agent keeps a distinct
-- persona. Additive (claw-rwire-only) — rust-claw never reads it.
CREATE TABLE IF NOT EXISTS agent_prompts (
  agent_group_id TEXT PRIMARY KEY REFERENCES agent_groups(id),
  body           TEXT NOT NULL,
  updated_at     TEXT NOT NULL
);

-- First-class, shareable workspaces: a named directory under <data>/workspaces/<id> that a
-- team of agents works in (their workspace tools + the AGENT.md project brief). Each has an
-- owner agent (its domain lead + default task assignee); an agent owns at most one workspace
-- (its home). `owner_agent_id` is nullable in the column definition so it can be added to an
-- existing DB via ALTER (see db::migrate), but the app always sets it; the partial unique
-- index enforces the one-home-per-agent rule. Additive (claw-rwire-only).
CREATE TABLE IF NOT EXISTS workspaces (
  id             TEXT PRIMARY KEY,
  name           TEXT NOT NULL,
  owner_agent_id TEXT REFERENCES agent_groups(id),
  created_at     TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_workspaces_owner
  ON workspaces(owner_agent_id) WHERE owner_agent_id IS NOT NULL;

-- Workspace membership (M:N): which workspaces an agent is a member of — the teams whose
-- tasks it can be assigned and whose files it may touch. An agent may be a member of several
-- workspaces; the owner is implicitly a member of its own. Additive (claw-rwire-only).
CREATE TABLE IF NOT EXISTS agent_workspaces (
  agent_group_id TEXT NOT NULL REFERENCES agent_groups(id),
  workspace_id   TEXT NOT NULL REFERENCES workspaces(id),
  PRIMARY KEY (agent_group_id, workspace_id)
);

-- The task board: a queue of work cards owned by a workspace. Each workspace runs its tasks
-- one at a time (sequential); workspaces run in parallel. A task is assigned to a member
-- agent and executes in the workspace's directory. Additive (claw-rwire-only).
CREATE TABLE IF NOT EXISTS tasks (
  id              TEXT PRIMARY KEY,
  workspace_id    TEXT NOT NULL REFERENCES workspaces(id),
  agent_group_id  TEXT REFERENCES agent_groups(id),   -- assignee (a member); NULL = owner
  title           TEXT NOT NULL,
  spec            TEXT NOT NULL,                       -- the instruction the agent runs
  kind            TEXT NOT NULL DEFAULT 'oneshot',     -- oneshot | recurrent
  schedule        TEXT,                                -- cron (recurrent only)
  status          TEXT NOT NULL DEFAULT 'pending',     -- pending|running|done|failed|cancelled
  origin_chat_id  TEXT,                                -- the chat to notify on completion
  created_by      TEXT,
  result          TEXT,                                -- last run output
  next_run        TEXT,                                -- when a recurrent task is next due
  last_run        TEXT,
  priority        INTEGER NOT NULL DEFAULT 0,
  step_position   INTEGER NOT NULL DEFAULT 0,          -- current pipeline step (Phase F)
  iteration       INTEGER NOT NULL DEFAULT 0,          -- iteration within a loop step (Phase F)
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tasks_lane ON tasks(workspace_id, status, priority);

-- Intra-workspace dependencies: `task_id` can't start until `blocked_by` is done. Both must
-- share a workspace (cross-workspace blockers would couple the parallel lanes). A linear
-- chain is a pipeline. Additive (claw-rwire-only).
CREATE TABLE IF NOT EXISTS task_blockers (
  task_id     TEXT NOT NULL REFERENCES tasks(id),
  blocked_by  TEXT NOT NULL REFERENCES tasks(id),
  PRIMARY KEY (task_id, blocked_by)
);

-- A workspace's workflow pipeline (Phase F): an ordered list of steps a task flows through.
-- A `stage` step is one agent run; a `loop` step is a bounded produce<->validate cycle (F-2).
-- No rows for a workspace = the default single Execute stage (today's behaviour). Additive.
CREATE TABLE IF NOT EXISTS pipeline_steps (
  id                 TEXT PRIMARY KEY,
  workspace_id       TEXT NOT NULL REFERENCES workspaces(id),
  position           INTEGER NOT NULL,
  kind               TEXT NOT NULL DEFAULT 'stage',   -- stage | loop
  name               TEXT NOT NULL,
  agent_id           TEXT REFERENCES agent_groups(id),  -- producer/stage agent (NULL = owner)
  prompt             TEXT,                              -- role-preamble override (NULL = default)
  validator_kind     TEXT,                              -- agent | human (loop only)
  validator_agent_id TEXT REFERENCES agent_groups(id),
  validator_prompt   TEXT,
  max_iterations     INTEGER NOT NULL DEFAULT 1,
  on_exhaust         TEXT NOT NULL DEFAULT 'human'      -- human | fail
);
CREATE INDEX IF NOT EXISTS idx_pipeline_steps ON pipeline_steps(workspace_id, position);

-- The run history of a task as it flows through its pipeline (Phase F): one row per agent run,
-- the source of the chained context and the detail-view audit trail. Additive.
CREATE TABLE IF NOT EXISTS task_runs (
  id          TEXT PRIMARY KEY,
  task_id     TEXT NOT NULL REFERENCES tasks(id),
  position    INTEGER NOT NULL,             -- which pipeline step
  role        TEXT NOT NULL,                -- stage | producer | validator
  agent_id    TEXT,
  output      TEXT,
  verdict     TEXT,                         -- pass | revise | reject (validator runs, F-2)
  feedback    TEXT,
  iteration   INTEGER NOT NULL DEFAULT 0,
  ok          INTEGER NOT NULL DEFAULT 1,
  created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_task_runs ON task_runs(task_id, id);

CREATE TABLE IF NOT EXISTS connectors (
  id             TEXT PRIMARY KEY,
  kind           TEXT NOT NULL UNIQUE,
  label          TEXT,
  config         TEXT NOT NULL,
  agent_group_id TEXT NOT NULL REFERENCES agent_groups(id),
  enabled        INTEGER NOT NULL DEFAULT 1,
  created_at     TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS channel_cursors (
  channel_type TEXT PRIMARY KEY,
  cursor       INTEGER NOT NULL
);

-- Global key/value config (Phase E): editable defaults that override the hardcoded constants —
-- the role preambles and the memory tuning knobs. An absent/blank value falls back to the
-- compiled default. Additive (claw-rwire-only).
CREATE TABLE IF NOT EXISTS settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

-- Per-agent rolling memory (Phase D): a summarized, persistent recollection of the agent's
-- own thread, injected into its runs so context survives without unbounded transcript growth.
-- `summarized_through_id` is the last chat message folded into `summary` (the concierge's
-- thread); turns after it are still sent raw. Additive (claw-rwire-only).
CREATE TABLE IF NOT EXISTS agent_memory (
  agent_group_id        TEXT PRIMARY KEY REFERENCES agent_groups(id),
  summary               TEXT NOT NULL DEFAULT '',
  summarized_through_id INTEGER NOT NULL DEFAULT 0,
  paused                INTEGER NOT NULL DEFAULT 0,
  updated_at            TEXT NOT NULL
);
