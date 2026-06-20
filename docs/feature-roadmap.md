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
blocking model (worker threads, like `dispatch_native` already uses). Pull infrastructure
in feature-by-feature, letting each port add only what it needs.

> **But challenge the tokio assumption — it may not be inevitable.** The rewrite doc §2
> (which calls for a second tokio runtime) predates the *actual* bridge, which proved the
> seam works on `std::thread` + `mpsc` + `update_shared_changed` with **no second runtime**
> (`bridge/mod.rs`). Re-examined against that reality, none of the ports is strictly
> tokio-only: tool-calling needs only step 0 below; MCP stdio is newline-framed JSON-RPC a
> dedicated thread can drive with blocking pipes (rust-claw *uses* tokio but its `conn.rs`
> is just blocking-style read/write); SMS is a poll thread + `sleep`, and its webhook is a
> route, not a listener (§3.6). So **treat "introduce tokio" as an open decision, not a
> cost baked into MCP/SMS.** Carrying them on the existing thread+mpsc seam keeps the "one
> impedance mismatch, isolated" promise even cleaner than the rewrite doc imagined. The
> honest first infra step is step 0 — *runs own a DB connection* — not *stand up tokio*.

**Step 0 — WAL + run-owned connection (do before tool-calling).** Today the bridge owns
one `central.db` connection and is the only writer (`bridge/mod.rs`, `db/mod.rs:23` sets
no pragmas). A multi-round tool loop breaks that: each round does blocking HTTP *and*
blocking tool exec *and* must persist, so the loop can't live on the bridge thread (it
would block the command inbox) — it lives on the worker, which then needs **its own
connection**. Two writers on `central.db` means `SQLITE_BUSY` unless WAL is on. So:
enable `journal_mode=WAL` + a `busy_timeout` in `db::open`, and let a run hold a
short-lived connection for its duration. This is the small, concrete infra step that
unblocks tool-calling — cheaper and more certain than the tokio question above.

**Second through-line — one new plugin seam carries three features.** rust-claw exposes
`ChannelAdapter` and `AgentProvider` as `dyn` seams. Add a **`Tool` seam of the same
kind**: built-in tools, MCP tools, and SMS tools all become implementors. That single
abstraction underpins items 4 (tool calling), 5 (MCP), and the SMS tools of item 7.

### 1.1 The session-model pivot (decide before MCP/agent-to-agent/SMS)

The current transcript (`web_messages`) is keyed by **`messaging_group_id`, not
`session_id`** — and the `sessions` table (`schema.sql:94`), though present, is unused.
That difference is invisible for chat and tool-calling but load-bearing for the second
half:

- **Agent-to-agent (§3.5):** rust-claw namespaces a durable *worker session* by the
  caller's session id (`delivery.rs:343`), so a sub-agent accumulates context across
  delegations. A `messaging_group`-keyed ledger has nowhere to put that — our
  synchronous sub-run is therefore *stateless per delegation*, a real divergence to own.
- **Multi-agent wiring:** `messaging_group_agents` allows several agents on one chat; a
  `messaging_group`-keyed transcript can't say *whose* turn each row is.
- **SMS:** routing creates/reuses sessions; cursors are per channel.

**Decision to make explicitly, with this cut line:** tool-calling (§3.1) and workspaces
(§3.2) ship on the **current implicit model** (a "session" = the current turn's message
list; transcript stays `messaging_group`-keyed). MCP, agent-to-agent, and SMS should
**start using the `sessions` table** as the per-(agent, chat) ledger, with a
session-aware transcript projection. Naming this now keeps steps 1–2 from baking in
assumptions that steps 3–5 must tear out.

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
  trait, feed results back, repeat to a cap. Persist calls + results so they **survive in
  the transcript** (our ledger *is* the transcript, so this fixes rust-claw's cross-turn
  gap). **Persistence shape (important — a `tool` direction string is too coarse):** an
  OpenAI-compatible replay needs an assistant message carrying a `tool_calls` array
  (`{id, function:{name, arguments}}`) followed by `tool` messages whose `tool_call_id`
  matches — a rendered string like `🔧 bash → exit 0` can't reconstruct that. So:
  - reuse the **existing `web_messages.kind` column** (`schema.sql:147`, default `'chat'`,
    **no migration**) with `kind='tool_call'` / `'tool_result'`;
  - store the structured payload (call id, name, arguments JSON, result) in a **new
    additive sidecar table** (e.g. `web_message_tools`) — additive tables are schema-safe
    per §0; **never `ALTER web_messages`** (it's format-shared with rust-claw);
  - split the three concerns: **live loop** uses real `tool_calls`/`tool` messages in the
    in-memory list; **display** renders persisted rows as the cards below; **cross-turn
    replay** flattens prior tool rounds into a *compact text summary* appended to the prior
    assistant turn, rather than replaying raw `tool`-role messages — because
    `agent::build_messages` (`agent/mod.rs:52`) coalesces adjacent roles and drops trailing
    assistant turns, and injecting tool-role rows into that pipeline is fragile.
- **UX improvement:** render tool activity inline as **collapsible cards** in the thread —
  `🔧 bash · cargo test → exit 0 (12 lines)`, expandable to full output, with
  running/done/failed status (reuse the Activity colour tokens). A `tool_profile` toggle
  in the agent drawer. The "thinking…" spinner becomes "running bash…".
- **Architecture improvement:** the `Tool` `dyn` seam (built-in / MCP / SMS implementors);
  persisted tool turns; both a per-turn round cap *and* a per-run wall-clock budget
  surfaced on the Activity board.
- **Port these rust-claw invariants verbatim — cheap correctness:**
  - **`produced_message` guard** (`providers/native/mod.rs:141`): once a tool has emitted a
    user-visible message, a *later* round failing must **not** fail/retry the whole turn
    (else duplicate sends, double assistant rows). Port the moment a message-emitting tool
    exists.
  - **Tool errors return to the model as strings, never abort the turn**
    (`tools.rs:313`). A "failed" card = the tool returned an error string; the loop
    continues. UX cards and loop semantics must agree on this.
  - **Caps:** rust-claw uses `MAX_TOOL_ROUNDS = 6` + 300s completion timeout + 120s bash
    timeout. Keep the round cap; add the wall-clock budget with a concrete default (e.g.
    5 min) surfaced on Activity.

### 3.2 Filesystem / workspaces

- **rust-claw:** per-agent-group `folder` under `/data/groups/<folder>/`; all sessions of
  a group share it; an `AGENT.md` in the folder is the system prompt. Scoped **by
  convention only** (their §11: "not enforcement"; Docker is the real boundary).
- **claw-rwire architecture:** `agent_groups.folder` already exists — and
  `db/agents.rs:103` already sets **`folder = id`** for every agent (`NOT NULL UNIQUE` in
  `schema.sql:21`), so a folder *always exists*; there is no "none" case to model.
  **Correction to the earlier "one or none / nullable folder" idea:** making `folder`
  nullable is a column change on a shared table — forbidden by §0 (breaks opening a
  rust-claw `central.db`) — *and* unnecessary. Instead: always scaffold
  `…/workspaces/<folder>/`, and **gate `bash`/`read`/`write`/`edit` on `tool_profile`
  (chat vs coder)** — exactly what rust-claw does (`providers/native/tools.rs:50`; coder
  tools added only when `profile == Coder`). The `tool_profile` column already exists
  (`schema.sql:28`). `read`/`write`/`edit` resolve relative to the workspace; `bash` runs
  with `cwd` there.
- **UX improvement (also resolves the earlier per-agent-prompt request):** store the
  agent's instructions as **`AGENT.md` inside its workspace**, edited as a "System prompt"
  textarea in the agent drawer. This sidesteps the shared-schema column blocker entirely
  (it's a file, not a column) *and* matches rust-claw. Add a read-only **Files view**:
  browse an agent's workspace as a tree with file preview.
- **Architecture improvement:** make path-scoping for `read`/`write`/`edit` **enforced**
  (reject `..` and absolute escapes), not conventional. rust-claw **already wrote the
  enforcement** in `workspace/path.rs::jail` (lexical normalization + symlink
  canonicalization) — it just doesn't apply it in the provider's file tools. **Port that
  function directly** rather than re-deriving it. Keep the optional sandbox seam (run
  `bash` under `bwrap` / a per-agent container when configured) at the spawn point —
  rust-claw explicitly left this hook open.

### 3.3 Web search

- **Reality:** the running session is already connected to the **Web Search Hacks MCP**
  (`search` / `fetch` / `screenshot` / `interact`). rust-claw bundles its own
  `mcp-web-search-stdio`.
- **Recommendation (revised — build it first, not last):** the plan originally filed web
  search under MCP (step 3, which drags in the heaviest infra). But a thin built-in
  `web_search` `Tool` needs only the seam from step 1, and the running session already has
  the Web Search Hacks MCP as a backend. So make it the **first tool built on the new
  seam**: it validates the `Tool` trait end-to-end with something *useful* before bash/fs,
  and decouples "useful agent" from "tokio." Keep the attached-MCP-server version
  (§3.4) as the eventual replacement, with no special-casing.

### 3.4 MCP support

- **rust-claw:** hand-rolled stdio JSON-RPC client, **one global hardcoded** "web" server,
  no per-agent config, no auth, **no reconnect** on crash.
- **claw-rwire architecture:** generalize the `Tool` seam to MCP. Add an **`mcp_servers`
  table** (additive → schema-safe) and an agent↔server join (reuse the
  `messaging_group_agents` pattern). stdio transport first (child process + JSON-RPC).
  rust-claw frames this as newline-delimited JSON-RPC over blocking-style read/write
  (`mcp/conn.rs`); a **dedicated supervisor thread** can drive it (spawn child, blocking
  pipes, reconnect on EOF/crash) without a second runtime — so this is *not* necessarily
  "the feature that pulls tokio in" (see §1). Decide thread-vs-tokio here deliberately.
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
  (rust-claw lets any agent call any group — a footgun, and has *no* recursion guard at all:
  confirmed in `delivery.rs`) and visible recursion-depth limits. **Share one wall-clock
  budget across the whole delegation tree**, not per-agent — a synchronous sub-run makes
  A→B→C a stack of blocking calls, so a per-agent budget would grant a 3-deep chain 3× the
  time. One budget for the run tree, alongside the depth guard.

### 3.6 SMS

- **rust-claw:** `ChannelAdapter` trait; SMS via a **sim-server** (polling + a webhook
  *nudge*, cursor-based dedup via `channel_cursors`, GSM-7/ASCII degradation, approvals
  kept web-only because SMS senders are spoofable). Not a real carrier.
- **claw-rwire architecture:** introduce the `ChannelAdapter` `dyn` seam. SMS adapter:
  inbound (webhook **or** poll) → `Command::Inbound` → route via a
  `messaging_group_agents` wiring → run → reply via the adapter's `deliver`. `connectors`
  and `channel_cursors` already exist. **Correction to the "separate listener" claim:** the
  webhook is plain HTTP, but that does *not* force a second listener. Two facts: (1) rwire's
  gate already serves `GET /logout` — a plain, non-page route — so rwire can host plain HTTP
  handlers via `Server::…routes(r)`; (2) in rust-claw the SMS webhook is **not** a separate
  listener either — it's folded into the main HTTP app and just calls `wake()` on the poller
  (`channels/sms/mod.rs:82`). So **make the webhook one more route on the existing port**
  (verify `routes()` accepts an arbitrary POST handler first), not a new socket. This
  removes the other claimed tokio trigger (see §1).
- **UX improvement:** SMS as a **second channel in the System Map** (beside Web) — a
  connector drawer (provider, base URL, token, webhook secret), route-to-agent, enable
  toggle, and a last-message / health line. SMS threads appear in the **chat sidebar with
  an SMS badge**, readable and replyable from the operator UI.
- **Architecture improvement over rust-claw:** a **real provider** behind the trait
  (Twilio / Vonage / …) rather than a sim; the inbound webhook hardened as a route on the
  existing listener; per-connector routing through the existing wirings; port their
  ASCII-degradation logic.

---

## 4. Recommended build order

Revised from the original (changes flagged): **web search pulled forward** to the seam
debut, a **step 0** (WAL) pulled ahead of tool-calling, **tokio demoted** from an assumed
cost to an open decision, and the **session-model pivot** named before MCP.

| # | Step | Real cost / brings in | Status |
|---|------|-----------------------|--------|
| A | Logout | 1 line (`Link` → `/logout`) | ✅ done |
| A′ | Archive | command pair + projection field + `⋯` menu | ✅ done — promoted to a dedicated `/archived` screen |
| **0** | **WAL** (§1) | `journal_mode=WAL` + `busy_timeout` in `db::open` | ✅ done (run-owned connection deferred — see below) |
| 1 | Tool seam + persisted tool turns + web search | `Tool` seam, blocking MCP stdio client | ✅ done — `src/mcp/`, `src/tools/`, `agent::run_turn`, `web_message_tools` sidecar, inline tool cards. Verified e2e (search→fetch→answer) against the live endpoint + `mcp-web-search-stdio` |
| 2 | Workspaces + `AGENT.md` prompt | workspace scaffolding | ✅ done — `src/workspace/` (`jail()` ported + enforced), `src/tools/{coder,exec}` (bash/read/write/edit), **per-tool selection** (`agent_tools` table + drawer toggle-chips, replacing the chat/coder profile), `AGENT.md` system prompt. Verified e2e (selected coder tools wrote+read a file; web tools gated off) |
| 3 | **Session-model decision (§1.1)**, then generalised MCP | thread *or* tokio — decide here | session pivot named; tokio no longer assumed |
| 4 | Agent-to-agent | sync sub-run on sessions | shared tree-budget + depth guard + allowlist |
| 5 | SMS | `ChannelAdapter`, webhook **route** (not listener), real provider | listener assumption dropped |

**Step 1 — what shipped vs. deferred.** Shipped: blocking MCP stdio client (one hardcoded
web server, mirroring rust-claw), the `Toolbox` seam, the multi-round loop (`MAX_TOOL_ROUNDS`
+ wall-clock budget), tool turns persisted via a `web_message_tools` sidecar + `kind='tool'`
rows (cross-turn replay folds them to text), and inline tool cards. Deferred (not blocking):
live per-round streaming + the **run-owned DB connection** (the worker returns the full turn
and the bridge persists at the end — Option B), the `produced_message` guard (no
message-emitting tools yet), per-card collapse toggle (cards use a capped scroll box for now),
and generalised multi-server MCP (Step 3).

**Step 2 — what shipped vs. deferred.** Shipped: `src/workspace/` (ported `jail()` —
lexical + symlink canonicalization — now *enforced* on `read`/`write`/`edit`, the one
improvement over rust-claw, which leaves its file tools unjailed), `scaffold`/`AGENT.md`
read+write+compose; `src/tools/{coder,exec}` (the four coder tools, `bash` with a
deadlock-free 120s timeout); `CompositeToolbox` + `FilteredToolbox`; `AGENT.md` as the
system prompt; and the agent drawer's **per-tool picker** + System-prompt textarea.

**Tool selection (supersedes the chat/coder profile).** The initial `tool_profile` toggle
was replaced by explicit **per-tool selection**: each agent's enabled tools live in the
additive `agent_tools` table (the shared `tool_profile` column is left untouched for
rust-claw compat). The drawer shows toggle-chips grouped Web/Coder; an unconfigured agent
defaults to web tools on (preserving Step 1), and the run filters *both* coder and web
tools to the enabled set via `FilteredToolbox`.

**Files view (ported from rust-claw, browse + edit).** A `/files` tab: a two-pane workspace
manager (`src/ui/files.rs`) over `src/workspace/ops.rs` (ported `list`/`read_text`/`write_text`/
`mkdir`/`delete`/`rename`, all jailed, 1 MiB text cap + binary detection). Pick an agent →
browse its workspace (breadcrumb + listing), open a file into an editor, **save edits**, and
create/rename/delete files+folders — each a `Command` on the bridge (`src/bridge/files.rs`)
projecting into `App.files_*`. Verified e2e (browse → open → edit+save → create → rename →
delete, all reflected on disk). Deferred: **upload/download** (binary transfer needs dedicated
HTTP routes); `bash` sandboxing (`bwrap`/container — seam left at the spawn point); the
`produced_message` guard (no message-emitting tools yet).

**Open decisions to settle before step 3** (don't block step 2): (B) the session-model cut
line in §1.1, and (C) tokio-vs-thread for generalised MCP/SMS in §1.

Throughout, the rewrite plan's invariant holds: every feature is still **a `Command`, a
pipeline step, and a projection back into `App`** — the same shape as the chat core that
already works.
