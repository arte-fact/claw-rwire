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
