# Workflow pipelines — per-workspace agent chains with validation loops (Phase F)

> Today a task is **one** agent run → done. Phase F makes a workspace's workflow a configurable
> **pipeline**: an ordered list of **steps**, where a step is either a **stage** (one agent run)
> or a **bounded validation loop** (produce ⇄ validate until it passes or hits a cap). A task
> flows through the pipeline, accumulating context; "review" stops being a special lifecycle and
> becomes *a loop whose validator is an agent and/or a human*. The Tasks tab becomes a **kanban**
> whose columns are the pipeline's steps, and each card opens a **detail view**. Everything is
> per-workspace and leans on the role-prompt system (Phase E).

## 1. The model

```
A workspace PIPELINE = an ordered list of STEPS.
A STEP is one of:
  • STAGE  — one agent run (executor / drafter / publisher …). output → next step.
  • LOOP   — a bounded validation cycle:

        ┌────────────────────────────────────────────────┐
        │  loop up to N times:                            │
        │     PRODUCE  ← producer agent + role prompt,    │
        │               runs with the latest feedback     │
        │     VALIDATE ← validator (agent OR human) +     │
        │               role prompt → verdict(...)        │
        │        pass   → exit loop → next step            │
        │        revise → feedback ⟶ PRODUCE, iterate      │  ← the real loop edge
        │        reject → fail (terminal)                  │
        │  on exhaust (N without pass) → human gate | fail │  ← termination guard
        └────────────────────────────────────────────────┘
```

- **Every stage / producer / validator is "an agent + a role prompt."** The role prompt is what
  makes it behave as executor / refiner / critic — *the prompt guides it* (Phase E made these
  editable and settings-overridable).
- **A loop genuinely iterates:** the validator's `feedback` is fed back into the producer, which
  *re-does the work*, and it repeats until `pass` or the iteration budget runs out. A linear
  chain can't express "repeat until valid"; the loop step can.
- **Review is not special.** Auto-review = a loop whose validator is an agent. User review = a
  loop whose validator is a human gate. They compose:
  `[Execute] → loop{ Refine ⇄ Critic(agent), max 2 } → [Approve(human)] → done`.

## 2. The verdict — how a decision leaves the model

A validator must emit a machine-readable decision so the engine can route — **not** by grepping
prose (which is exactly what failed with deepseek's text-leaked tool calls). So a validator run
gets a tiny **`ValidatorToolbox`** with one tool:

```
verdict(decision: "pass" | "revise" | "reject", feedback: string)
```

The validator's role prompt instructs it to read the actual output/files, judge against the
spec, and **call `verdict`**. It runs with **read-only** file tools — it inspects the output and
the workspace, it doesn't rewrite them. A human gate produces the same verdict from a UI action
(Approve = pass, Request changes = revise + notes, Reject = reject). Same construct, different
validator.

## 3. Context flow

Each run sees: the task **spec** + the **prior runs' outputs** (the chain so far) + (for a
producer re-run) the validator's latest **feedback**. Assembled with `build_messages`; if the
chain gets long, the rolling-memory summarizer (Phase D) condenses it. Every run is recorded
(`task_runs`) so the chain is replayable and auditable.

## 4. The engine (how it runs)

Generalizes today's executor. A task carries a **current step** + (inside a loop) an
**iteration** count.

- On a producer/stage **`TaskFinished`**: append the run to `task_runs`.
  - stage step → advance to the next step (or `done` if last).
  - loop step, producer finished → run the **validator**.
- On a validator **`TaskFinished`** (verdict parsed from the `verdict` tool call):
  - `pass` → exit loop → next step / `done`.
  - `revise` & `iteration < max` → `iteration++`, re-run producer with feedback.
  - `revise` & `iteration == max` → on-exhaust policy (human gate / fail).
  - `reject` → fail.
- A **human-gate** validator: the task parks; the user's action supplies the verdict.

Each run is a worker thread posting back (the existing `AgentReply`/`TaskFinished` shape).
Lane occupancy: one *active run* per workspace at a time (unchanged); a task parked at a human
gate frees the lane.

## 5. Kanban (the Tasks tab)

Columns **are** the pipeline steps (+ a terminal `Done` and a collapsible `Closed` for
failed/rejected/cancelled). A task sits in its current step's column. A **loop is one column**;
its cards show an **iteration badge** (`Refine 2/3`) and the latest verdict (⏳/✓/✎/✗) — you
watch a card *tighten in place* rather than ping-pong.

The Tasks tab has a persisted **view toggle: Kanban ↔ List**. List is today's flat, ordered
list — the mobile-friendly view on narrow screens; Kanban is the columns. The engine drives
movement either way (see §10 for why there's no drag-drop). The global dashboard gains a
"needs you: N" badge (cards parked at human gates).

## 6. Task detail view

Route `/w/:wid/tasks/:tid` (or a wide drawer):
- **Header** — title · current step · assignee · workspace · attempt/iteration.
- **Spec** — editable (today's edit-drawer fields fold in here).
- **Chain** — each run in order: which step/role, the agent, its output, and (validators) the
  verdict + feedback. The audit trail of the loop.
- **Output** — the latest/accepted output; links into Files for what changed.
- **Actions** — at a human gate: Approve / Request changes (notes) / Reject; always: Re-run ·
  Cancel · Edit.

## 7. Per-workspace configuration

A workspace's pipeline is fully editable (the **modular/configurable** requirement) on a
dedicated **Workflow** tab (alongside Overview / Files / Tasks / Team): add / remove / reorder
steps; set each step's kind (stage | loop), its **agent** (per step), its role prompt(s), and —
for loops — `max_iterations` + `on_exhaust`. Role-prompt editing reuses the Phase E controls.

**Editing is gated:** the pipeline can't be changed while the workspace has any in-flight task
(`pending` / `running` / `awaiting` / `review`) — so an edit can never disrupt a running task;
clear the lane first. (This is why no per-task pipeline snapshot is needed.)

Default pipeline for a new workspace = a single **Execute** stage → done (identical to today;
zero migration). Because agents are configured **per step**, the per-task assignee is superseded
— a task carries title + spec and flows through the pipeline, each step run by its configured
agent (the default Execute stage = the workspace owner).

## 8. Data model (additive — sketch, firmed up in F-1)

- `pipeline_steps(id, workspace_id, position, kind[stage|loop], name,
   producer_agent_id, producer_prompt,
   validator_kind[agent|human], validator_agent_id, validator_prompt,
   max_iterations, on_exhaust[human|fail])` — nullable columns unused by the step's kind.
- `task_runs(id, task_id, position, role[stage|producer|validator], agent_id, output,
   verdict, feedback, iteration, created_at)` — the chain history + context source.
- `tasks` gains `step_position INTEGER DEFAULT 0`, `iteration INTEGER DEFAULT 0`; `status` gains
  `awaiting` (parked at a human gate) and `rejected`.
- Agents are per **step** (`pipeline_steps.producer_agent_id` / `validator_agent_id`), so
  `tasks.agent` is retired from routing (left nullable / unused) and `create_task` drops its
  `agent` param — the workspace pipeline decides who runs each step.

## 9. Builds on what exists

- **Phase E role prompts** — a stage/producer/validator is an agent + a role prompt; add
  `producer` / `validator` run-kinds; the Prompts inspector + settings overrides come along.
- **Scheduler** — `TaskFinished` routes through the engine (§4) instead of straight to done;
  reviewer/producer runs reuse the worker pattern.
- **Notify (Phase C-3b)** — accept/reject rides the concierge notify turn into the chat.
- **Memory (Phase D)** — condenses a long chain's context.

## 10. UX — surfaces & interaction model

**The board moves itself.** Tasks advance automatically — the engine walks the pipeline. The
operator *never drags a card between steps*; their touchpoints are **create**, **act at a human
gate**, **edit / cancel / re-run**, and **configure the pipeline**. So there's no drag-drop (the
no-JS constraint is moot) — the board is a **live monitor**, not a manipulation surface. Four
jobs:

| Job | "…" | Surface |
|---|---|---|
| **Monitor** | what's happening | the board + dashboard |
| **Act** | what needs *me* | the **review inbox** + gate actions |
| **Understand** | what happened | the task **detail / chain** |
| **Configure** | how this workspace works | the **pipeline editor** |

**Board view toggle (Kanban ↔ List).** A persisted toggle on the Tasks tab. *Kanban* = columns
by step (desktop); *List* = today's flat ordered list — the mobile default. The user switches
freely.

**Review inbox — a dedicated nav surface.** The primary "act" surface: one place listing *every*
task across all workspaces parked at a human gate, newest first, with a **count badge in the
nav** (reinforced by a concierge chat notify). Without it, gates get lost. Each row opens the
review actions.

**Gate actions on the card *and* the detail view.** A one-click **Approve** on the card for the
common case; Request-changes (notes) / Reject and the rest in the detail view.

**Detail view = a transcript.** The run chain (produce → validate → produce → …) renders like
the existing chat transcript (reused), with a verdict chip between rounds — the loop "tightening"
reads as a scrollable thread. Output rendered inline; file writes link into Files.

**Cross-cutting.** Terminal/mono/small-text language + the existing card/badge/status widgets.
The **"needs you" signal** appears in the nav badge, the dashboard, and chat, so human gates
can't hide.

---

## ☐ Phase F — roadmap (tick as it lands; gate each: `fmt --check` · `clippy -D` · `cargo test`)

**F-1 — Pipeline model + linear engine** _(done; no loop yet)_
- [x] Schema: `pipeline_steps` + `task_runs` + `tasks.step_position`/`iteration` (migration);
      default = one implicit Execute stage (`db::pipeline::steps_for`).
- [x] `db::pipeline` (steps_for / insert_stage) + `db::task_runs` (append / list).
- [x] Engine (`scheduler`): `launch_step` runs the current step's agent (step agent → assignee
      → owner) with the step's role preamble + the **chained context** (spec + prior outputs);
      `finished` records the run and **advances** (`next_action`) — next stage or done; failure
      ends the task; recurrent done resets to step 0. Default single-stage tasks unchanged.
- [x] Showcase: Studio seeded with `Execute(Forge) → Refine(Forge)`; Research keeps the default.
- [x] 129 tests (steps_for default, task_runs, next_action, step_context) + **e2e against a mock
      endpoint**: a Studio task chained through both stages (2 task_runs) → done.

**F-2 — Validation loop (the core)** _(done)_
- [x] `loop` step kind (producer + agent validator + `max_iterations` + `on_exhaust`); engine
      runs producer → validator; `RunRole` (Stage/Producer/Validator) on `TaskFinished` routes it.
- [x] `tools::validator::ValidatorToolbox` — the `verdict(decision, feedback)` tool;
      `RunKind::Validator` + `VALIDATOR_PROMPT` (settings-overridable via `role.validator`);
      validator runs **read-only** (workspace `read` + `verdict`). `parse_verdict` reads the
      decision from the run's invocations.
- [x] Engine loop logic (`route_verdict`): pass → advance past the loop (done if last); revise →
      re-run producer with feedback (iteration++ up to `max_iterations`), then `on_exhaust`
      (`fail` / `awaiting`); reject → terminal. A missing verdict defaults to revise (bounded).
      `TaskStatus` gained `awaiting`/`rejected`; `tasks.iteration`.
- [x] 133 tests (verdict parsing + tool validation; next_action) + **e2e (mock endpoint)**:
      pass → producer→validator(pass)→done; revise → producer/validator ×2 → exhaust → fail.

**F-3 — Human gates + review inbox (user review + action)** _(done)_
- [x] `validator_kind = human`: a producer parks in `awaiting` instead of running an agent
      validator; `scheduler::human_verdict` records the user's decision as a validator run and
      drives the *same* `route_verdict` (revise re-runs the producer; a human reviewer is never
      capped). `tasks::list_awaiting`/`count_awaiting`.
- [x] **Review inbox** — `/review` nav surface (`ui/review.rs`) listing every parked task across
      workspaces with the work to review + Approve / Request-changes(notes) / Reject
      (`Command::ReviewVerdict`); a live **count badge** on the Review nav row (`review_badge`,
      refreshed each tick + on park/resolve); a cheap (non-LLM) chat ping on first park.
- [x] 133 tests + e2e: a human-gate loop parks `awaiting` → shows in the inbox with its output
      → **Approve** → `done`; chain = producer → validator(pass, human).

**F-4 — Kanban + view toggle + task detail view**
- [ ] Tasks tab gains a persisted **Kanban ↔ List** toggle (List = today's view, mobile default).
- [ ] Kanban: columns = pipeline steps (+ Done / Closed); loop = one column + iteration/verdict
      badge; one-click **Approve on the card** (full actions in detail).
- [ ] Detail route `/w/:wid/tasks/:tid`: spec, the run **chain** as a transcript (audit trail),
      output, all gate + task actions.
- [ ] Dashboard "needs you: N" badge. e2e.

**F-5 — Per-workspace pipeline editor**
- [ ] Workflow tab (or Team-tab section): add / remove / reorder steps; set kind, agent(s), role
      prompts, `max_iterations`, `on_exhaust`. Reuses Phase E prompt-editing controls.
- [ ] Validation (no empty pipeline; a loop needs a producer + validator). e2e.

**F-6 — Prompts inspector + showcase**
- [ ] Inspector gains the producer/validator run-kinds (visible + tunable defaults).
- [ ] Showcase: seed **Studio** with `Execute(Forge) → loop{ Refine(Forge) ⇄ Critic(agent), max 2 }
      → Approve(human) → done`, and a seeded **Critic** reviewer agent, so the whole loop is
      visible on first boot. (Other workspaces keep the default single-stage pipeline.)

---

## Decisions

- [x] **Shape** — pipeline = linear list of steps; a step may be a **bounded validation loop**.
      No arbitrary DAG branching (deferred).
- [x] **Verdict** — via a `verdict` tool, not prose parsing.
- [x] **Default pipeline** — a single Execute stage (migration-free; today's behaviour).
- [x] **Review = a loop** — agent validator (auto) and/or human gate (you); they compose.
- [x] **The board is a live monitor** — the engine moves cards; no drag-drop. User touchpoints:
      create · act-at-gate · edit/cancel/rerun · configure.
- [x] **Review inbox** — a dedicated **nav surface** (cross-workspace, count badge), the primary
      "act" surface.
- [x] **Board view** — a user-switchable **Kanban ↔ List** toggle (List = mobile default).
- [x] **Gate actions** — on the **card** (one-click Approve) *and* the detail view.
- [x] **Agents are per-step** — each step configures its own agent; the per-task assignee (and
      `create_task`'s `agent` param) is superseded. Default Execute stage = the workspace owner.
- [x] **Pipeline edits are gated** — a pipeline can't be edited while the workspace has any
      in-flight task (`pending`/`running`/`awaiting`/`review`); clear the lane first. So an edit
      can't disrupt a running task — no per-task snapshot needed.
- [x] **Default validator** = a **human gate** (you sign off); the showcase seeds a **Critic**
      agent to demo the auto path.
- [x] **Editor home** = a dedicated **Workflow** workspace tab (alongside Overview/Files/Tasks/Team).
- [x] **Validator file access** = **read-only** (inspects output, doesn't rewrite it).

**All design decisions are resolved — F-1 is unblocked.**

## Out of scope (later)

- Arbitrary branching / non-linear DAGs; parallel stages within a single task.
- Cross-workspace pipelines; sharing a pipeline template across workspaces.
- A fully model-driven validator (no explicit verdict — "edit until satisfied"); the `verdict`
  tool is the chosen mechanism, but a free-form variant could be a later option.
