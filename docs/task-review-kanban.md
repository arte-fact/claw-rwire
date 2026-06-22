# Draft — task review stage, kanban board, detail view

> Tasks stop jumping execution → done. After a task runs it enters **Review**: a dedicated
> **Reviewer agent** auto-checks the output against the spec and posts a verdict, then **you**
> accept it, send it back for changes (a rework loop), or reject it. The Tasks tab becomes a
> **kanban** (columns by stage); each card opens a **detail view** with the spec, the full
> output, the review thread, and the actions. Builds on the role-layer system (a 4th run-kind),
> the scheduler, settings, and the Team tab.

## 1. The lifecycle (new)

```
            ┌──────────────── rework (changes requested) ──────────────┐
            ▼                                                            │
 pending ──► running ──► review ──► (auto-review verdict) ──► [you decide] ─┤
  ▲  │                     │                                              │
  │  └ blocked (deps)      └ auto-review off → straight to your decision  │
  │                                                                       │
  └── recurrent: accepted → reschedule → pending          accept ─► done  │
                                                          reject ─► rejected (terminal)
```

States (the kanban columns map to these):

| Status | Column | Meaning |
|---|---|---|
| `pending` (blocked) | **Blocked** | has unfinished blockers (derived) |
| `pending` (ready) | **Ready** | runnable — queued for the scheduler |
| `running` | **Running** | executing now (≤1 per workspace) |
| `review` | **Review** | output produced; auto-review and/or your sign-off pending |
| `done` | **Done** | accepted |
| `failed` · `rejected` · `cancelled` | **Closed** (collapsible) | execution error / you rejected / cancelled |

`review` carries a sub-signal (auto-review running vs verdict-ready vs awaiting-you), shown on
the card, not as separate statuses — keeps the enum small.

## 2. The Reviewer role (4th run-kind)

We already layer prompts by **run-kind** (Chat / Task / Planning → role preamble). Add
**Review**. A `REVIEWER_PROMPT` (settings-overridable, like the others in E-3):

> "You are reviewing completed work against its task spec. Judge correctness, completeness,
> and adherence to the workspace's conventions. Read the actual files/output, don't assume.
> Output a verdict — **approve** / **changes** / **reject** — and concise, actionable notes."

The reviewer **reads the workspace** (it gets the file tools, read-only-ish) + the task spec +
the assignee's result, and returns a structured verdict. It's a *dedicated agent role*: a
workspace designates a **reviewer** (a member; default = the owner, or a seeded "Critic"
agent). The review run is `run-kind = Review` → the Prompts inspector gains a 4th chip.

## 3. The flow

1. **Execution finishes** (today's `TaskFinished`). Instead of → done:
   - auto-review **on** → status `review`, spawn a **reviewer run** (worker, like notify) →
     verdict + notes saved.
   - auto-review **off** → status `review` (await you), no agent run.
2. **Auto-review verdict lands:**
   - `approve` + *require-human* off → **auto-accept** → `done` (+ concierge notify).
   - otherwise → stays in **Review**, verdict shown, waiting for you.
3. **You decide** (in the detail view):
   - **Accept** → `done` (+ notify). Recurrent → reschedule.
   - **Request changes** (with a note) → **rework**: back to `pending`, `attempt++`, the review
     notes appended as run context; the assignee re-runs, then re-reviews.
   - **Reject** → `rejected` (terminal).
4. **Guardrail:** cap auto-rework rounds (e.g. 2); after that it parks in Review for you, so a
   model can't loop forever. `log()` when the cap is hit.

Lane throughput: a task in `review` **frees the execution lane** (the scheduler runs the next
task); the reviewer run is its own worker, so review never blocks execution.

## 4. Kanban UI (the Tasks tab)

Columns (Blocked · Ready · Running · Review · Done · Closed▸) as horizontal lanes that scroll;
each a vertical stack of compact cards (title · assignee · ⟳/⏱ · blocker count). The **Review**
column's cards show the auto-verdict badge (✓ approve / ✎ changes / ✗ reject / ⏳ reviewing) and
are the visually prominent ones.

**Movement is via contextual actions, not drag-drop** — rwire has no JS, so HTML5 drag-drop
isn't available. Each card/column exposes the moves that make sense there (a Review card →
Accept / Changes / Reject; a Ready card → Run-now / Edit / Cancel). The detail view is where the
heavier actions live. (Drag-drop = a later item, needs framework support.)

The **global dashboard** all-lanes view gains a "needs review: N" badge per workspace.

## 5. Task detail view

A card opens a detail route `/w/:wid/tasks/:tid` (or a wide drawer). Sections:

- **Header** — title · status · assignee · workspace · kind/schedule · attempt N.
- **Spec** — the instruction (editable here = today's edit drawer fields, folded in).
- **Blockers** — links to blocker tasks + their status.
- **Output** — the full result (not the card snippet); for file-writing tasks, links into the
  Files view for what changed.
- **Review** — the auto-review verdict + notes; a **human review** panel: Accept / Request
  changes (notes box) / Reject; and the **review history** (each round: auto + human, verdict,
  notes, who, when).
- **Run history** — last_run, attempts, any errors.
- **Actions** — Re-run · Cancel · Edit.

## 6. Data model (additive)

- `tasks.status` gains `review`, `rejected`; `tasks.attempt INTEGER DEFAULT 0`;
  `tasks.rework_notes TEXT` (the latest change request, fed into the re-run).
- `task_reviews(id, task_id, kind TEXT[auto|human], verdict TEXT[approve|changes|reject],
  notes TEXT, reviewer TEXT, created_at)` — the review thread (history of both kinds).
- Reviewer designation + policy, per workspace: either a small `workspace_config` table or
  settings keys namespaced by workspace —
  `review.<wid>.auto` (bool), `review.<wid>.require_human` (bool), `review.<wid>.reviewer`
  (agent id). Edited on the **Team tab**.

## 7. Builds on what exists

- **Roles/prompts:** Review is just a 4th run-kind + a settings-overridable preamble (E-3).
- **Scheduler:** `TaskFinished` routes to review instead of done; a reviewer worker mirrors the
  notify/summarize worker shape; rework re-queues as `pending`.
- **Notify:** accept/reject can ride the existing concierge notify turn into the chat.
- **Team tab:** the per-workspace review policy + reviewer designation lives here.
- **Prompts inspector:** add the Review chip so the reviewer prompt is visible/tunable.

## 8. Suggested phasing

- **R-1 — Kanban + detail view** (no review yet): re-lay the Tasks tab as columns by current
  status; add the detail route. Pure UI re-org over today's model. Ships value immediately.
- **R-2 — Human review:** add `review`/`rejected` statuses, the `task_reviews` table, the
  detail-view review panel (Accept / Changes / Reject), the rework loop + attempt cap. Tasks
  land in Review for your sign-off (no agent yet).
- **R-3 — Auto-review (the Reviewer role):** the `REVIEWER_PROMPT` run-kind, per-workspace
  reviewer + policy, the reviewer worker, auto-accept-on-pass. The verdict pre-fills your review.

## 9. Open forks (need your call)

- **Default gating** — when a task finishes, should it (a) wait for *you* every time, (b)
  auto-accept if the Reviewer approves and only stop for you on changes/reject, or (c) be
  configurable per workspace (recommend c, default to b)?
- **Who reviews** — (a) a dedicated **seeded "Critic" agent** per team, (b) the **owner**
  reviews its members' work, or (c) configurable reviewer per workspace (recommend c, default
  to the owner; the showcase seeds a Critic)?
- **Where to start** — R-1 (kanban first, immediate UI win) or straight to the review pipeline?
