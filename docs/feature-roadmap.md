# Feature roadmap — porting the rest of claw onto rwire

> Companion to `rust-claw/docs/rewrite-on-rwire.md` (the rewrite plan). That doc covers
> the chat/admin/dashboard core, already built. This one covers **what's still missing**
> versus the rust-claw reference, with a proposed UX *and* architecture for each. Grounded
> in a read of both codebases (rust-claw at `/home/artefact/rust-claw`, this project at
> `/home/artefact/claw-rwire`) — not guesses.

---

## 0. Where claw-rwire stands today (the baseline these build on)

claw-rwire is deliberately **much simpler** than rust-claw right now:

- **Web channel only.** No `sessions`, `router`, `supervisor`, `delivery`, or per-session
  ledgers. Chat is one `web_messages` table (`messaging_group_id, direction, sender,
  body`) in `central.db`.
- **Chat-only provider.** `agent::complete` does a single blocking `/chat/completions`
  call via `ureq`. No tool calling.
- **One blocking bridge thread.** `std::thread` owning `central.db` + a `Command` inbox,
  projecting into shared `App`. No tokio, no child processes, no polling loops.

But the **schema already mirrors rust-claw's full central schema**: `sessions`,
`connectors`, `channel_cursors`, `messaging_group_agents`, and
`agent_groups.folder/tool_profile/cli_scope` all exist and sit unused. So most of the
work below is **runtime logic, not migrations**. (Adding *new* tables is also safe — the
schema is `CREATE TABLE IF NOT EXISTS` and format-shared with rust-claw; adding *columns*
to existing shared tables is not, because it breaks opening a rust-claw `central.db`.)

Two facts that make the "missing" features cheap:

- rwire's auth gate already serves **`GET /logout`** (clears the token cookie).
- `messaging_groups.archived_at` exists and `list_chats` already filters
  `archived_at IS NULL`.

---

## 1. The cross-cutting architectural decision (read before any port)

Four of the five ports need **concurrency the current bridge doesn't have**: SMS polls a
provider, MCP spawns and supervises child processes, agent-to-agent runs overlap, and a
tool loop does blocking IO in the middle of a turn. rust-claw solves this with a tokio
pipeline (`router · queue · supervisor · delivery · providers · channels · mcp`). The
rewrite plan §2 already calls for exactly this shape: **a tokio subsystem behind the
`Command` channel that projects into `App`.** Our current `bridge` is the seed of it.

**Decision:** don't build the whole pipeline up front. Keep tool-calling on the present
blocking model (worker threads, like `dispatch_native` already uses). **Grow the bridge
into the tokio pipeline when the first genuinely-concurrent feature lands (MCP or SMS),**
letting each feature pull in only what it needs.

**Second through-line — one new plugin seam carries three features.** rust-claw exposes
`ChannelAdapter` and `AgentProvider` as `dyn` seams. Add a **`Tool` seam of the same
kind**: built-in tools, MCP tools, and SMS tools all become implementors. That single
abstraction underpins items 4 (tool calling), 5 (MCP), and the SMS tools of item 7.

---

## 2. Quick wins (shippable now, no pipeline work)

### 2.1 Logout

- **Architecture:** none — rwire's gate already handles `GET /logout`.
- **UX:** a "Log out" affordance in the sidebar. Either a small footer link under
  Conversations, or fold the brand-row status into a tiny user menu (`claw ▾` → Log out).
  Implemented as a single `Link` to `/logout`.

### 2.2 Archive conversations

- **Architecture:** `Command::ArchiveChat(id)` / `UnarchiveChat(id)` set/clear
  `messaging_groups.archived_at`, then re-project `App.chats`. The active-list query
  already excludes archived rows.
- **UX (better than rust-claw, which just hides):** a hover `⋯` menu per chat row
  (Archive), plus a collapsible **"Archived"** section at the bottom of the sidebar
  (lazy-loaded via a command) offering **Restore**. Confirm-on-archive only when the
  thread is non-empty.

---

## 3. The five ports

Each section: *what rust-claw does → claw-rwire architecture → UX improvement →
architecture improvement over rust-claw.*

### 3.1 Tool calling — the keystone (do this first)

- **rust-claw:** native loop, `MAX_TOOL_ROUNDS = 6`, OpenAI `tools`/`tool_calls`,
  profile-gated (`chat` vs `coder`). Built-ins: `bash`/`read`/`write`/`edit` (coder),
  plus `send_message`, `schedule_task`, `ask_user_question`. Known gap (their PLAN 4.7):
  **tool rounds are lost across turns** — the model re-derives intermediate state.
- **claw-rwire architecture:** grow `agent::complete` from single-shot into a
  **multi-round loop** — send `tools`, parse `tool_calls`, dispatch through a `Tool`
  trait, feed results back, repeat to a cap. Add a `tool` direction to `web_messages` so
  calls + results **persist in the transcript** (our ledger *is* the transcript, so this
  fixes rust-claw's gap for free).
- **UX improvement:** render tool activity inline as **collapsible cards** in the thread —
  `🔧 bash · cargo test → exit 0 (12 lines)`, expandable to full output, with
  running/done/failed status (reuse the Activity colour tokens). A `tool_profile` toggle
  in the agent drawer. The "thinking…" spinner becomes "running bash…".
- **Architecture improvement:** the `Tool` `dyn` seam (built-in / MCP / SMS implementors);
  persisted tool turns; both a per-turn round cap *and* a per-run wall-clock budget
  surfaced on the Activity board.

### 3.2 Filesystem / workspaces

- **rust-claw:** per-agent-group `folder` under `/data/groups/<folder>/`; all sessions of
  a group share it; an `AGENT.md` in the folder is the system prompt. Scoped **by
  convention only** (their §11: "not enforcement"; Docker is the real boundary).
- **claw-rwire architecture:** `agent_groups.folder` already exists. Scaffold
  `…/workspaces/<folder>/` on agent create. **"One or none"** = nullable folder; an agent
  without a workspace simply isn't offered `bash`/filesystem tools. `read`/`write`/`edit`
  resolve relative to the workspace; `bash` runs with `cwd` there.
- **UX improvement (also resolves the earlier per-agent-prompt request):** store the
  agent's instructions as **`AGENT.md` inside its workspace**, edited as a "System prompt"
  textarea in the agent drawer. This sidesteps the shared-schema column blocker entirely
  (it's a file, not a column) *and* matches rust-claw. Add a read-only **Files view**:
  browse an agent's workspace as a tree with file preview.
- **Architecture improvement:** make path-scoping for `read`/`write`/`edit` **enforced**
  (reject `..` and absolute escapes), not conventional. Keep the optional sandbox seam
  (run `bash` under `bwrap` / a per-agent container when configured) at the spawn point —
  rust-claw explicitly left this hook open.

### 3.3 Web search

- **Reality:** the running session is already connected to the **Web Search Hacks MCP**
  (`search` / `fetch` / `screenshot` / `interact`). rust-claw bundles its own
  `mcp-web-search-stdio`.
- **Recommendation:** don't hand-roll a web-search tool — make it the **first MCP server**
  (§3.4). One path: web search is just an attached MCP server, no special-casing. If it's
  wanted before full MCP, a thin built-in `web_search` `Tool` over the same backend is a
  fine one-day placeholder.

### 3.4 MCP support

- **rust-claw:** hand-rolled stdio JSON-RPC client, **one global hardcoded** "web" server,
  no per-agent config, no auth, **no reconnect** on crash.
- **claw-rwire architecture:** generalize the `Tool` seam to MCP. Add an **`mcp_servers`
  table** (additive → schema-safe) and an agent↔server join (reuse the
  `messaging_group_agents` pattern). stdio transport first (child process + JSON-RPC) —
  **this is the feature that pulls tokio in**, for async child IO and a supervised client
  with reconnect.
- **UX improvement:** MCP servers become a **fourth System Map column**
  (Endpoints · Agents · MCP · Channels) — add/configure/enable a server in the drawer, see
  its **discovered tools**, attach it to agents like Web→agent routing, with **health
  badges** (connected/failed) and a reconnect button.
- **Architecture improvement over rust-claw:** per-agent attachment (they're global-only);
  reconnect + health; **streamable-HTTP transport** for remote servers (the claude.ai-style
  ones); credential storage reusing the endpoints `api_key` / `api_key_env` pattern.

### 3.5 Communication between agents

- **rust-claw:** a `send_to_agent` tool → pseudo-channel `agent` → delivery spins a
  **worker session** namespaced by the caller's session → reply returns via
  `agent-return` and is written back as inbound to the originator. It's **1:1 delegation**
  (not group orchestration); any agent may call any group; the operator sees flat system
  separators; there's no topology UI.
- **claw-rwire architecture:** once tool-calling + a minimal session/run concept exist,
  add a `send_to_agent` `Tool`. Minimal version on today's model: a **synchronous
  sub-run** (call the target agent, capture its reply, feed it back) with a depth/loop
  guard. Project delegations into Activity as **child runs** and into the transcript.
- **UX improvement:** make the **delegation graph first-class in the System Map** —
  directed edges showing who-can-call-whom — and render delegations in the thread as
  **nested, collapsible cards** (`claw → Coder: "review this"` with the sub-exchange
  inline), rather than rust-claw's flat separators.
- **Architecture improvement:** an explicit **allowlist** of permitted delegations
  (rust-claw lets any agent call any group — a footgun) and visible recursion-depth limits.

### 3.6 SMS

- **rust-claw:** `ChannelAdapter` trait; SMS via a **sim-server** (polling + a webhook
  *nudge*, cursor-based dedup via `channel_cursors`, GSM-7/ASCII degradation, approvals
  kept web-only because SMS senders are spoofable). Not a real carrier.
- **claw-rwire architecture:** introduce the `ChannelAdapter` `dyn` seam. SMS adapter:
  inbound (webhook **or** poll) → `Command::Inbound` → route via a
  `messaging_group_agents` wiring → run → reply via the adapter's `deliver`. `connectors`
  and `channel_cursors` already exist. Key constraint the rewrite plan §9.1 flagged: the
  **webhook is plain HTTP, not an rwire page** — run it on a **separate listener owned by
  the channel subsystem** (this, with MCP, is what brings tokio in).
- **UX improvement:** SMS as a **second channel in the System Map** (beside Web) — a
  connector drawer (provider, base URL, token, webhook secret), route-to-agent, enable
  toggle, and a last-message / health line. SMS threads appear in the **chat sidebar with
  an SMS badge**, readable and replyable from the operator UI.
- **Architecture improvement over rust-claw:** a **real provider** behind the trait
  (Twilio / Vonage / …) rather than a sim; the inbound webhook hardened on its own
  listener; per-connector routing through the existing wirings; port their
  ASCII-degradation logic.

---

## 4. Recommended build order

| # | Step | Brings in | Notes |
|---|------|-----------|-------|
| A | Logout + Archive | — | Hours. No pipeline work. |
| 1 | Tool-calling foundation | `Tool` seam, persisted tool turns | The keystone; agents become useful. |
| 2 | Workspaces + fs/bash + `AGENT.md` prompt | workspace scaffolding | Also delivers per-agent system prompts. |
| 3 | MCP | **tokio**, supervised stdio clients | Web search lands here as the first server. |
| 4 | Agent-to-agent | minimal sessions/runs | Builds on tool-calling. |
| 5 | SMS | `ChannelAdapter`, webhook listener, real provider | Heaviest; needs the tokio pipeline. |

Throughout, the rewrite plan's invariant holds: every feature is still **a `Command`, a
pipeline step, and a projection back into `App`** — the same shape as the chat core that
already works.
