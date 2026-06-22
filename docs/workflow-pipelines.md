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
spec, and **call `verdict`**. A human gate produces the same verdict from a UI action
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
watch a card *tighten in place* rather than ping-pong. Movement is via contextual card actions
(no drag-drop; rwire has no JS). The global dashboard gains a "needs you: N" badge (cards parked
at human gates).

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

A workspace's pipeline is fully editable (the **modular/configurable** requirement). A
**Workflow** tab (or a Team-tab section): add / remove / reorder steps; set each step's kind
(stage | loop), its agent(s), its role prompt(s), and — for loops — `max_iterations` +
`on_exhaust`. Role-prompt editing reuses the Phase E controls. Default pipeline for a new
workspace = a single **Execute** stage → done (identical to today; zero migration).

## 8. Data model (additive — sketch, firmed up in F-1)

- `pipeline_steps(id, workspace_id, position, kind[stage|loop], name,
   producer_agent_id, producer_prompt,
   validator_kind[agent|human], validator_agent_id, validator_prompt,
   max_iterations, on_exhaust[human|fail])` — nullable columns unused by the step's kind.
- `task_runs(id, task_id, position, role[stage|producer|validator], agent_id, output,
   verdict, feedback, iteration, created_at)` — the chain history + context source.
- `tasks` gains `step_position INTEGER DEFAULT 0`, `iteration INTEGER DEFAULT 0`; `status` gains
  `awaiting` (parked at a human gate) and `rejected`.

## 9. Builds on what exists

- **Phase E role prompts** — a stage/producer/validator is an agent + a role prompt; add
  `producer` / `validator` run-kinds; the Prompts inspector + settings overrides come along.
- **Scheduler** — `TaskFinished` routes through the engine (§4) instead of straight to done;
  reviewer/producer runs reuse the worker pattern.
- **Notify (Phase C-3b)** — accept/reject rides the concierge notify turn into the chat.
- **Memory (Phase D)** — condenses a long chain's context.

---

## ☐ Phase F — roadmap (tick as it lands; gate each: `fmt --check` · `clippy -D` · `cargo test`)

**F-1 — Pipeline model + linear engine** _(no loop yet)_
- [ ] Schema: `pipeline_steps` + `task_runs` + `tasks.step_position`; default = one Execute stage.
- [ ] `db::pipeline` (read a workspace's steps) + `db::task_runs` (append/list).
- [ ] Engine: on `TaskFinished`, append the run, advance through **stage** steps (output →
      next step's context), `done` after the last. Existing single-stage tasks behave identically.
- [ ] Seed/insert a 2-stage pipeline (Studio: `Execute → Refine`) to exercise real chaining.
- [ ] Tests (engine advance, context flow) + e2e (a 2-stage task runs both, failure path).

**F-2 — Validation loop (the core)**
- [ ] `loop` step kind (producer + agent validator + `max_iterations` + `on_exhaust`).
- [ ] `tools::validator::ValidatorToolbox` — the `verdict(decision, feedback)` tool; `VALIDATOR`
      run-kind + preamble (settings-overridable).
- [ ] Engine loop logic: iteration counter, feedback → producer re-run, route on verdict, cap +
      on-exhaust. Auto-review works end to end.
- [ ] Tests (verdict routing: pass/revise/reject; cap → on-exhaust; verdict-tool parsing).

**F-3 — Human gates (user review + action)**
- [ ] `validator_kind = human`: task parks in `awaiting`; user action (Approve / Request changes
      / Reject) supplies the verdict and drives the same loop (revise → re-run producer).
- [ ] Minimal action surface first (buttons on the card / a thin panel), full detail view in F-4.
- [ ] Tests (human verdict advances/loops/fails) + e2e.

**F-4 — Kanban + task detail view**
- [ ] Tasks tab → columns = pipeline steps (+ Done / Closed); loop = one column + iteration/verdict
      badge; contextual card actions.
- [ ] Detail route `/w/:wid/tasks/:tid`: spec, the run **chain** (audit trail), output, actions.
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
- [ ] **Default validator** when a workspace adds review — *human gate* (you sign off) vs a
      *seeded Critic agent*. Lean: human gate by default; the showcase seeds a Critic to show the
      auto path. Resolve before F-3/F-6.
- [ ] **Editor home** — a new **Workflow** tab vs a section on the **Team** tab. Resolve before F-5.

## Out of scope (later)

- Arbitrary branching / non-linear DAGs; parallel stages within a single task.
- Cross-workspace pipelines; sharing a pipeline template across workspaces.
- A fully model-driven validator (no explicit verdict — "edit until satisfied"); the `verdict`
  tool is the chosen mechanism, but a free-form variant could be a later option.
