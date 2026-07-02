# Agent-centric threads (draft)

> Status: **draft / proposal** — a direction to react to, not a committed plan. Grounded in
> what's already in the tree so it reads as an evolution, not a rewrite.

## The shift

Today the app is **chat-centric**: the left nav is a list of *chats*, you start a *new chat*
with the concierge, and the workers (Forge, Scout, Critic) are invisible — they run headless
inside tasks and you only meet their output on a board. Conversations are disposable; the agents
doing the work are not addressable.

The proposal flips the unit of navigation and conversation from **chat** to **agent**:

- **No "new chat."** There is one *persistent* thread per agent — the concierge included — that
  never resets. History accrues; the agent's memory is the summarized tail of its own thread.
- **The top nav is an agent list, not a chat list.** Each row is an agent you can open, follow,
  and talk to: its status (idle / working / awaiting you), a live pulse when it's active, a badge
  when it needs approval.
- **Each agent has a dedicated page — its thread** — where you watch what it's doing in real time,
  approve its tasks inline, and leave it a message that it picks up. One place to *follow* and
  *steer* an agent, instead of three disconnected surfaces (concierge chat, task Peek, Activity
  feed).

The agent becomes a long-lived collaborator with a continuous working relationship, rather than a
function you invoke through a throwaway chat.

## Why now — most of the parts already exist

This is less new construction than **unification** of things we just built:

| Capability the vision needs | What's already in the tree |
|---|---|
| Persistent per-agent memory across tasks | `agent_memory` (rolling `summary` per `agent_group_id`) + `memory_layer` injected into every run's system prompt (`bridge/scheduler.rs`) |
| A thread that interleaves messages, work, and live tool activity | `ui/thread.rs` (`scroll` + authored `entry`), the task **Thread** tab (`ui/task_detail.rs`), and the in-order `live_turns` stream + isolated `live_entry` renderer (just landed) |
| Follow an agent's tool calls in real time | `live_turns` (per-task ordered text↔tool stream) and the global `live_activity` feed (`ui/activity.rs`) |
| Approve / reject / request-changes inline in a thread | the review gate in the Peek Thread (`review_gate`, `approve`/`reject`/`request_changes`) |
| Route a request to the right agent | the concierge `ask`/Delegate tool (`tools/concierge.rs`) |
| Continue an agent's own conversation across passes | `bridge/replay.rs` — per-role thread reconstruction (producer/validator continuity) |

The agent thread is essentially the **task Thread generalized to the agent's whole life**, and the
nav change is swapping the chat-thread list for an agent list. The hard new piece is the
**interaction model** (leaving a message that a working agent picks up), covered below.

## Information architecture

**Nav (left rail) — agents, grouped by workspace.**

```
Concierge                 ● (always first — the one you route through)
─ Studio ───────────
  Forge      ▷ working   (2)   ← live pulse + "2 awaiting your approval"
  Critic     idle
─ Research ──────────
  Scout      ⏸ awaiting  (1)
─────────────────────
Dashboard · Activity · Logs · System        (app-level surfaces stay)
```

Agents are members of workspaces (`agent_workspaces`), so the natural grouping is *workspace →
its agents*, with the workspace-less **Concierge** pinned on top. The current `workspaces_nav`
becomes an **agents_nav**: each row deep-links to the agent's thread and shows a status dot + an
approval badge (reuse the `awaiting`-per-workspace projection, re-keyed per agent).

**Pages.**

- `/agent/:id` — the agent thread (new; the centerpiece).
- `/` → the Concierge thread (the concierge is just the default agent).
- Workspaces, boards, workflow, files, Activity, Logs, System, Dashboard **stay** as they are —
  the board is still the best *cross-agent* view of a workspace's tasks; the agent thread is the
  best *single-agent* view. They're complementary lenses on the same tasks.

## The agent thread (one unified timeline)

An agent's page is a single autoscrolling thread (reuse `thread::scroll` + `thread::entry`)
whose entries, in time order, are any of:

1. **User messages** to the agent ("also handle the empty-state", "ship it", "why did you pick X?").
2. **Agent replies** — its prose, streamed live via `live_turns`.
3. **Task lifecycle** — "started *Build a color highlighter*", each run as an authored, phase-tagged
   entry with collapsible tool rows (exactly today's `run_entry`), the verdict chips, feedback.
4. **The live run** — the isolated `live_entry` renderer, streaming text↔tool in order.
5. **Approval gates** — when one of the agent's tasks parks `awaiting`, the inline
   approve / reject / "what to change…" composer appears *in the thread* at that point.

So the concierge thread reads as a conversation; a worker's thread reads as "chatter + the tasks it
ran + what it's doing right now" — the same vocabulary, different mix. Older history folds into the
agent's `agent_memory` summary, which is what the agent actually carries forward — the thread is the
human-readable projection of that continuity.

## Context model — roles, scope, budget (the keystone)

> Agreed 2026-07-02, after the first thread-page build missed the mark. This section is the
> foundation the redo builds on: **the thread page is an agent's context made visible**, so the
> context model has to come first.

**Principle: an agent's context mirrors its responsibility scope — memory horizon = decision
horizon.** Each role is defined by four things: its **conversation unit** (what one live thread
covers, full verbatim fidelity), its **rolling horizon** (what folds into working memory and
carries forward), its **scope key** (the partition the memory lives in), and its **reset
boundary** (where continuity intentionally breaks).

| Role | Conversation unit | Rolling horizon | Scope key | Resets |
|---|---|---|---|---|
| **Concierge** | its one persistent conversation | its whole life (older turns fold into summary) | global (one row) | never |
| **Producer** (Forge) | one task, across loop iterations | its workspace's tasks; **last task kept warm** | (agent, **workspace**) | never in-workspace; hard wall between workspaces |
| **Owner-as-planner** | one planning turn | same row as its producer memory | (agent, workspace) | never |
| **Validator** (Critic) | the task's **whole produce⇄review loop**, across iterations | none *beyond* the loop — fresh eyes per deliverable | task (attempt) | at task end, **not inside the loop** |

- **Concierge — global, conversational, forever.** Recent tail verbatim, everything older
  condensed into the rolling summary. Delegation outcomes land in its thread as notifications, so
  macro vision across workspaces accrues *by construction* — it gets outcomes, never the workers'
  forensics. Optionally a live **status digest** layer (lanes running/awaiting) injected fresh each
  turn: current state is looked up, never remembered. This model *requires* the single persistent
  thread — today's per-agent `summarized_through` marker applied across multiple "new chats" is
  incoherent (a real latent bug); the collapse fixes the memory mechanics.
- **Producer — workspace-scoped, sequential, three tiers.** At task launch: (1) the current task's
  own thread verbatim, across iterations (`replay.rs`, shipped); (2) the **last completed task's
  recap** kept warm — spec → outcome → verdict → files touched — because sequential tasks chain;
  (3) everything older folded into a rolling **workspace-scoped** summary (layout, conventions,
  decisions), folded **at task completion** (the task is the worker's natural turn boundary, as the
  message is the concierge's). Scope key is (agent, workspace): an agent in two workspaces holds
  two independent memories; nothing leaks across.
- **Owner-as-planner — same memory, different hat.** Planning turns read/write the same
  (agent, workspace) row: plans build on what shipped and what failed. Only the role preamble
  differs.
- **Validator — task-scoped, deliberately amnesiac beyond the task.** Within the loop, full
  continuity: its own reviews replayed across iterations, so re-reviews are incremental (check the
  delta, re-run the tests — don't re-inspect from scratch). This is what makes the produce⇄correct
  loop *converge cheaply*, and it's already shipped (`replay.rs` validator thread + the re-review
  focus). Across tasks: nothing — accumulated review memory breeds anchoring and rubber-stamping,
  and freshness makes cross-workspace reviewers (Critic) leak-proof by construction. **The loop is
  shared but the threads are not**: producer and validator each keep their *own* task-scoped
  thread, touching only at the output ↔ feedback boundary — the validator judges the work, not the
  effort; the producer receives feedback, not the reviewer's inner deliberation. A manual restart
  is a new attempt → the validator's context resets with it (`list_for_task_attempt` already
  enforces this). Delta from today: **remove the memory-layer injection into validator runs**.
  Later, optionally: a tiny curated per-workspace *standards memo* — rules, not a work log.

### Budget-aware composition

Memory is the **elastic** layer — identity can't compress, working headroom must not compress, so
memory absorbs all the sizing pressure. That's why it fills *last*.

```
C  = context window of the agent's (endpoint, model)   — discovered via /models, cached
R  = output reserve (the reply/stream)
B  = C − R                                              — the prompt budget

B  =  F  +  U  +  W  +  M          fill order: F → U → W → M   (M = remainder, floored at 0)

F  = fixed identity        role preamble + persona + capped brief      — never trimmed
U  = current unit          this turn / this task-thread replay         — the work itself
W  = working headroom      room for the tool rounds to come            — reserved BEFORE memory
M  = memory fill           recap, tail, rolling summary                — the only elastic layer
```

```
 ┌──────────────────────────────────┐ ─── B  (per endpoint: 8k model ≠ 128k model —
 │ F  role + persona + brief (cap)  │         same rule, different absolutes)
 ├──────────────────────────────────┤
 │ U  the current unit, verbatim    │  ← grows during the run…
 ├──────────────────────────────────┤
 │ W  ░░ working headroom ░░        │  ← …by eating W (never M mid-run;
 │    (tool results, iterations,    │     memory is fixed at launch)
 │     the streamed reply)          │
 ├──────────────────────────────────┤
 │ M  1. warm slot (recap / tail)   │  ← remainder only, filled by value:
 │    2. rolling summary (capped)   │     recency first, then compression
 └──────────────────────────────────┘        (allocation, not prompt order)
```

Per-role shares — roles differ in how much room the *work* needs:

| | Concierge | Producer | Validator |
|---|---|---|---|
| **W** headroom | ~30% of B | **~50%** (long build loops) | **~60%** (reads tree + runs builds) |
| **M** ceiling | ~40% | ~20% | ~0 — its **U *is* its memory** (the loop, verbatim) |
| **M** fill order | verbatim tail → summary | last-task recap → workspace summary | — |
| **Fold trigger** | after a turn, `prompt_tokens > 0.6·B` (exists) | **at task completion**; mid-task if U alone passes ~50%·B | none — the unit dies with the task |

**Degradation ladder** (small B): (1) shrink tail/recap to one-liners; (2) re-summarize the summary
to a hard cap (M ≤ ~10%·B); (3) **mid-unit fold** — a loop outgrowing U+W compacts its oldest
iterations into a unit-local recap (oldest reviews → one-line verdict+feedback), latest iteration +
current output stay verbatim; (4) never trimmed: role, persona, current spec, latest turn — if
those alone exceed B, surface a model-too-small error rather than degrade around it. An agent on
8k and the same agent on 128k run the *same rule* — the small one remembers less and folds sooner;
behaviour degrades continuously, never off a cliff.

**Mechanics** (mostly existing hooks): `CONTEXT_CACHE` resolves B per (endpoint, model) via
`/models`; token accounting estimates at compose time (≈ chars/4) and corrects with the endpoint's
reported `prompt_tokens` (the chat path's 0.6 trigger already does this — generalize per role);
today's `MEMORY_WINDOW`/`SUMMARIZE_FRACTION` constants become the per-role table above. New
invariant: compose in F→U→W→M order everywhere (today the memory layer is injected budget-blind).

**Deltas from today**: (1) `agent_memory` PK → (agent_group_id, workspace_id), NULL workspace =
the concierge's global row; (2) a producer-side fold writer at task completion (today only the
chat path summarizes — worker memory is permanently empty); (3) injection matrix: chat/task/
planning runs get their *scoped* memory, validators none; (4) producers get the last-task recap
block; (5) one persistent concierge thread (Phase 5's essence, pulled early).

**The page rule that falls out: an agent's page = its context made visible.** The concierge page
is its one conversation (the chat, composer included). A producer's page is its workspace lane as
a conversation — a "folded into memory" line at the summary boundary, the last-task recap, the
live task streaming below: what you see ≈ what Forge remembers. A validator's page is an activity
index at most — there *is* no continuous cross-task thread to show, and pretending otherwise is
why the first build read as a meaningless pile of runs for Critic.

## Interaction model — follow, approve, steer

Three verbs, in increasing difficulty:

- **Follow** (done): the live stream already renders in order and repaints in isolation. Opening an
  agent's page and watching it work is a projection we already have.
- **Approve** (mostly done): surface the existing review gate inside the agent thread when a task of
  that agent is `awaiting`. Re-key the `awaiting` projection from *per-workspace* to *per-agent*.
- **Steer — "leave a user message in its thread"** (the genuinely new piece). A message dropped into
  a working agent's thread has to reach the agent. Two honest options:
  - **v1 — turn-boundary queue (recommended first).** The message is appended to the agent's thread
    and delivered as the next user turn the agent sees: for an idle agent, it starts a turn now; for
    a mid-task agent, it's injected at the **next pass boundary** (the producer's next iteration, or
    the next run the scheduler launches). This composes cleanly with `replay.rs` — a queued user
    message is just another turn in the reconstructed thread. No interrupt, bounded latency, no
    partial-work corruption.
  - **v2 — mid-run interrupt.** Deliver the message into the *current* `run_loop` between tool
    rounds (a checked channel the loop drains each round), so the agent can change course
    immediately. More powerful, but needs care: the run is a blocking loop today, and cancel/restart
    semantics (epochs) must not double-run. Defer until v1 proves the ergonomics.

Message-in-flux is what turns "watch the agent" into "work *with* the agent."

## Data-model sketch

- **Agent thread store.** A per-agent, append-only message log (user + agent turns) — either a new
  `agent_messages(agent_group_id, direction, body, kind, created_at, …)` table, or repurpose the
  existing `messaging_groups`/`web_messages` by binding a group to an agent instead of a channel.
  Task/run/activity entries are *projected* into the thread from `tasks`/`task_runs`/`live_turns`,
  not copied — the thread view is a merge over these sources, ordered by time.
- **Memory.** `agent_memory` already summarizes an agent's tail; extend its `summarized_through_id`
  to walk the agent thread log so memory = "everything older than the live window," making
  cross-task persistence explicit and inspectable (a "memory" affordance on the agent page).
- **Concierge as an agent.** Fold the concierge into the same agent list/thread machinery; its
  distinguishing trait is only that it holds the `ask`/Delegate tool. "New chat" and `/chat/:id`
  retire in favor of the single concierge thread.

## Reused vs. new

**Reused (large fraction):** `thread.rs` widgets, `live_turns` + `live_entry`, `run_entry`/tool
rows, the review gate + handlers, `replay.rs` turn reconstruction, `agent_memory` + `memory_layer`,
the `awaiting` projection, `activity` describe/humanize helpers.

**New:** agents_nav (from `workspaces_nav`), the `/agent/:id` page + its merged timeline projection,
the per-agent message store + the "leave a message" composer/handler, the steer queue (v1), and
re-keying `awaiting` per agent.

## Open questions (decide before building)

1. **Direct-to-worker vs. concierge-only.** Can the user message Forge directly, or does the
   concierge still mediate all work? (The vision implies direct; Delegate then becomes one entry
   path among several.)
2. **Do workspaces/boards recede or stay co-equal?** Proposed: stay co-equal (board = cross-agent
   lens, thread = single-agent lens). Worth confirming.
3. **Steering semantics.** Queue-at-boundary (v1) vs. interrupt (v2) — start with v1?
4. **Thread scale.** A long-lived agent thread needs windowing (render the live tail, lazy-load
   history) — the `column-reverse` scroll pins to newest already, but history paging is new.
5. ~~**Approval routing.**~~ **Resolved (Phase 3):** an approval belongs to the *worker's* thread —
   the gate surfaces in the thread of the task's effective assignee, where the work was produced.

## Roadmap v2 (checklist) — rebuilt on the context model

> **Why a v2 (2026-07-02).** The v1 phases 1–3 shipped, but the thread page missed the mark — a
> read-only audit log (task markers + full forensic run entries), not a conversation, with an
> unbounded projection (all tasks × all runs × full tool transcripts, re-rendered wholesale per
> streamed token — the freeze). **Shipped and surviving:** the agent-list nav (concierge pinned,
> status/pulse, per-agent awaiting badges), the inline review-gate wiring (task-id-driven, works
> from any surface), the in-order `live_turns` stream, per-role loop continuity (`replay.rs`),
> `tasks::list_for_agent`. **Superseded:** the v1 thread page + its projection — redone in R2.

**Ordering rationale.** User-visible stabilization first — the pages are what's broken — and the
pages *specify* what memory must produce (a recap block, a fold line) before we build it, rather
than the backend landing invisibly and the pages faking it. R1 before R2 because the concierge
thread is the smallest right-feeling win (the chat surface already works, composer included), it
is the heart of the original ask ("no new chat — a persistent conversation"), and the single
thread is a *precondition* for coherent concierge memory (one thread, one `summarized_through`
marker). Steering stays last: it lands into a page that already has the right grammar.

Each phase is independently shippable and passes the full gate before the next. Tick items as they
land; keep this list the source of truth for progress.

### R1 — The concierge thread is the chat ✅
Pull the old Phase 5 to the front: one persistent conversation, no "new chat".
- [x] The concierge nav row opens the persistent conversation at `/` (chat glyph, sticky
      highlight whenever no other agent's page is open)
- [x] Collapse to **one** concierge thread: `chat::collapse_to_primary` at boot keeps the most
      recently *active* chat and archives the rest (reachable under Archived — listed + delete;
      Restore retired, since there is no active list to restore into)
- [x] `/chat/:id` resolves to the one thread (the route shows it; no per-id side-effect); retired
      `NewChat`/`OpenChat`/`ArchiveChat`/`UnarchiveChat` commands + the thread-list/new-chat UI
- [x] Memory coherence falls out: one thread ↔ one `summarized_through` marker (fixes the latent
      multi-chat marker bug)
- [x] `fmt`/`clippy`/`test` green (221 tests)

### R2 — Worker thread page, redone conversational ✅
Fix the freeze and the feel in one stroke. Pure projection over existing data — no new storage.
- [x] **Conversational grain**: `AgentTurn` — you (spec, tag "task") → agent (reply prose,
      phase tag, verdict chip) → you / reviewer (feedback; a human verdict's feedback IS the
      body, not the "(you) …" stub) — tool work as the muted "n tool calls · task ↗" meta line
      into the Peek; no transcripts carried in state
- [x] Strictly chronological merge of the lane + out-of-lane reviews
      (`task_runs::recent_by_agent`, capped in SQL); windowed to the newest 50 turns with a
      "· n earlier turns not shown ·" note (R3 turns it into the memory fold line)
- [x] **Rendering structure**: two sibling regions under static scaffolding — the keyed
      `timeline` (reads `agent_thread` only) + the `live_tail` (the only `live_turns` reader);
      no renderer nested in a keyed subtree; `refresh_if_open` builds **before** taking the
      shared-state lock (open agent mirrored bridge-side)
- [x] **Per-role shape emerges from the data**: a producer's thread = its lane as a conversation;
      a validator (no lane) = just its linked review turns — no fake continuous thread
- [x] Review gate stays inline at the tail; the Peek's tool-toggle key collision is gone (the
      thread renders no tool rows at all)
- [x] `fmt`/`clippy`/`test` green (224 tests, +3) — live smoke pass pending (operator)
- [x] **Post-R2 stabilization** (browser-verified): agent routes are workspace-scoped —
      `/w/:wid/agent/:aid` — so the thread URL mirrors the (agent, workspace) scope key and a
      shared agent (Critic) is a distinct thread per workspace; reading measure moved to `St`
      tokens (inline styles proved fragile across route swaps); rwire hardened (region
      null-guards, `data-route` full-nav fallback on a dead socket, synced-hash clear on swap)

### R3 — Memory mechanics (the substance) — ✅ shipped
The budget model from the keystone section, landing into pages that already have the right shape.
- [x] `agent_memory` re-key: PK (agent_group_id, workspace_id); the concierge's global row is
      `workspace_id = ''` (empty string, not NULL — a NULL in a composite PK never conflicts, so
      upserts would duplicate). In-place rebuild migration; legacy rows become the global row.
- [x] Producer fold **at task completion** (`scheduler::terminal_finish` →
      `memory::fold_finished_task`): the task recap is merged into each producing agent's
      (agent, workspace) row by a worker-thread summarizer sized to that agent's own M ceiling.
      Failures fold too — what broke is what the next task must remember.
- [x] Injection matrix: chat reads the global row, task/planning read their workspace's row
      (the Prompts panel mirrors the matrix and edits the row the run kind actually reads);
      **validator runs compose no memory layer** — loop continuity is the replayed thread alone
- [x] Last-task recap kept warm at producer launch (spec → outcome → verdict → files touched),
      matched by the run chain (`tasks::last_finished_for_agent`) so owner-defaulted work counts
- [x] Budget composition **F→U→W→M** (`bridge/memory.rs`): B = C − R from the cached `/models`
      context; shares (W/M): concierge+planner 30/40, producer 50/20, validator 60/0; memory
      fills last, by value — recap whole → recap headline → summary hard-truncated with a
      marker; **mid-unit fold** compacts a loop's oldest passes to one-line notes once the
      attempt outgrows ½·B, latest pass always verbatim
- [x] The producer page's truncation note is now the **fold line** ("older work condensed into
      memory — n earlier turns past the fold") with the memory card below it — the (agent,
      workspace) summary, editable in place (Save / Clear / Pause on that scope row); memory
      mutations re-project the open thread, so a background fold shows up live
- [x] `fmt`/`clippy`/`test` green (236 tests, +12); browser-verified: fold line + memory card
      on Forge · Studio, Pause/Resume round-trip scoped to the one row, legacy DB migrated in
      place on open

### R3.5 — Materialize memory (agreed 2026-07-02)
Memory acts invisibly today — folds overwrite the row in the background, and the summary is only
reachable where R3 put it (the Prompts panel, the agent-page card). This phase makes memory a
first-class citizen: every scope has a home on the page it governs, every change is an observable
event with provenance. Fold events render compactly (one muted line, like the fold line itself) —
the thread stays a conversation, with memory as its quiet narrator.
- [ ] **M1 — Memory journal (foundation)**: append-only `agent_memory_log` — every mutation
      (task fold, chat fold, operator edit / clear / pause) records (agent, workspace, kind,
      source, note, summary_after, created_at), pruned to the last ~50 per scope. The substrate
      M2/M3 read; buys future undo/diff for free.
- [ ] **M2 — Folds visible in the thread**: the agent timeline merges journal rows in
      chronologically — a muted system line ("condensed 'Build a color highlighter' into
      memory") at the moment the fold happened. A new `AgentThreadItem` variant; no new App
      fields. History becoming memory is *watchable* — the keystone's page rule, completed.
- [ ] **M3 — Concierge parity**: the chat page gets the fold line + the editable memory card
      for the **global** row (today it's only reachable via Prompts), plus M2's fold events.
      If message ids are already in the projection, the fold line sits at the exact
      `summarized_through` boundary inside the transcript; else top placement first.
- [ ] **M4 — Run provenance**: stamp each producer run with the memory layer it launched with
      (`task_runs.memory_snapshot`), shown in the Peek transcript — "what did Forge remember
      when it did that?". Cheap at launch time; costs storage per run.
- [ ] **M5 — Memory overview**: a System-page section (or `/memory`): every agent × scope with
      size, last-updated, paused flag, links to its pages. Admin view — worth it once the
      workspace count grows; last in line.
- [ ] `fmt`/`clippy`/`test` green per piece

### R4 — Steer: leave a message (v1: queue at turn boundary)
The moment it becomes agent-*centric*; unchanged from the old Phase 4.
- [ ] Per-agent message store; composer at the bottom of the worker thread
- [ ] Delivery: an idle agent starts a turn now; a mid-task agent picks it up at the **next pass
      boundary**, injected as the next user turn via `replay.rs`
- [ ] No corruption of in-flight work; bounded latency; test the queue→pickup path
- [ ] `fmt`/`clippy`/`test` green

### R5 — Later
- [ ] Mid-run steering (v2: drain a channel between tool rounds, respecting epoch/cancel)
- [ ] Validator standards memo (curated per-workspace rules — not a work log)
- [ ] Concierge live status digest layer (present state is looked up, never remembered)
- [ ] History beyond the window (lazy-load older timeline past the fold)

## Verification (when built)

Per phase, the usual gate (`cargo fmt --check && cargo clippy --all-targets -- -D warnings &&
cargo test`) plus a manual pass: open an agent mid-task, confirm the live stream flows and the
page never freezes when switching agents (R2); confirm a small-context endpoint folds sooner but
behaves the same (R3); and (R4) drop a message and confirm the agent picks it up on its next turn
without corrupting in-flight work.
