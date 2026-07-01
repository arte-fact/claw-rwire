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
5. **Approval routing.** With per-agent threads, does an approval still belong to the *workspace
   owner's* thread, or the *worker's* thread that produced the work? (Likely the worker's.)

## Roadmap (checklist)

Each phase is independently shippable and passes the full gate before the next. Tick items as they
land; keep this list the source of truth for progress.

### Phase 1 — Agent thread page, read-only
Prove the unified view as a pure projection over existing data.
- [ ] `/agent/:id` route + `agents_nav` deep-link target (stub the nav row for now)
- [ ] `AgentThread` projection: merge `agent_messages`(future) + that agent's `tasks`/`task_runs` +
      `live_turns`, ordered by time
- [ ] Render with `thread::scroll` + `thread::entry`; reuse `run_entry` + the isolated `live_entry`
- [ ] Task lifecycle entries ("started …", verdict chips) projected from `tasks`/`task_runs`
- [ ] Tests: projection ordering + entry mapping; `fmt`/`clippy`/`test` green

### Phase 2 — Nav becomes an agent list
- [ ] `workspaces_nav` → `agents_nav`: agents grouped by workspace, **Concierge pinned on top**
- [ ] Per-agent status (idle / working / awaiting) + live pulse dot
- [ ] Re-key the `awaiting` projection from per-workspace to **per-agent**; approval badge on the row
- [ ] Boards/workspaces stay reachable (secondary lens, not removed)
- [ ] `fmt`/`clippy`/`test` green

### Phase 3 — Inline approval in the agent thread
- [ ] Surface the existing `review_gate` in the agent thread when one of its tasks is `awaiting`
- [ ] Wire the existing `approve` / `reject` / `request_changes` handlers from the thread context
- [ ] Decide + implement approval home = the **worker's** thread (see open question 5)
- [ ] `fmt`/`clippy`/`test` green

### Phase 4 — Leave a message (v1: queue at turn boundary)
The moment it becomes agent-*centric*.
- [ ] Per-agent message store (`agent_messages` table **or** agent-bound `messaging_groups`)
- [ ] Composer at the bottom of the agent thread + submit handler (append user turn)
- [ ] Delivery: an idle agent starts a turn now; a mid-task agent picks it up at the **next pass
      boundary**, injected as the next user turn via `replay.rs`
- [ ] No corruption of in-flight work; bounded latency; test the queue→pickup path
- [ ] `fmt`/`clippy`/`test` green

### Phase 5 — Retire "new chat"
- [ ] Collapse the concierge into the agent model: one persistent concierge thread
- [ ] Remove "new chat" + `/chat/:id` (redirect `/` → concierge thread)
- [ ] Migrate/retire the chat-thread-list nav path
- [ ] `fmt`/`clippy`/`test` green

### Phase 6 — Later
- [ ] Mid-run steering (v2: drain a channel between tool rounds, respecting epoch/cancel)
- [ ] Memory affordance on the agent page (inspect/edit the `agent_memory` summary; extend
      `summarized_through_id` to walk the agent thread)
- [ ] Agent-thread history paging / windowing (render the live tail, lazy-load older history)

## Verification (when built)

Per phase, the usual gate (`cargo fmt --check && cargo clippy --all-targets -- -D warnings &&
cargo test`) plus a manual pass: open an agent mid-task, confirm the live stream, approve a parked
task from its thread, and (phase 4) drop a message and confirm the agent picks it up on its next
turn without corrupting in-flight work.
