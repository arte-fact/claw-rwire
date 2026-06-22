# Roadmap — prompt transparency & configurability (Phase E)

> Make the 4-layer system prompt **visible** and **tunable**. Today only 2 of 4 layers are
> editable (persona in System Map, brief in the workspace Overview), 2 are invisible (the role
> preamble and the rolling memory), nothing shows the *assembled* prompt, and the memory +
> thresholds are hardcoded. See the analysis in the conversation that produced this doc.

## The 4 layers (recap)

`bridge::run_system_prompt` → `workspace::compose_system_prompt(role, persona, brief, memory)`:

| # | Layer | Source | Scope | Editable today |
|---|---|---|---|---|
| 1 | Role preamble (`CONCIERGE_/EXECUTOR_/OWNER_PLANNER_PROMPT`) | consts in `agent/mod.rs` | global, per run-type | ❌ |
| 2 | Identity persona | `agent_prompts` | per-agent | ✅ System Map drawer |
| 3 | Workspace brief | `AGENT.md` | per-workspace | ✅ workspace Overview |
| 4 | Working memory | `agent_memory` | per-agent (auto) | ❌ |

Run-type → role: `dispatch_native`→concierge · `scheduler::launch`→executor · `orchestrate::ask`→owner-planner.
An agent (e.g. `ag-default`) can play all three. Also opaque: the `notify` + `summarize` prompts.

## Goal

1. **Transparent** — one place shows every layer *and the assembled result* for a chosen agent
   + run-type, so you can see exactly what the model receives.
2. **Configurable** — the invisible/hardcoded pieces (role preambles, memory, thresholds)
   become editable, with sensible defaults.

---

## ☐ Phase E — prompt transparency & configurability

**E-1 — Prompt inspector (read-only) + relabel** _(done, no schema change)_
- [x] `/prompts` view (`ui/prompts.rs`): agent `<select>` + run-kind chips (Chat / Task /
      Planning) → 4 labelled layers (Role / Persona / Brief / Memory) + an **Assembled**
      preview (reuses `compose_system_prompt`). Brief uses the agent's home workspace.
- [x] `Command::LoadPrompt { agent, run }` → `bridge/prompts.rs` composes read-only (no
      scaffold) + projects `App.prompt_preview` (`RunKind`/`PromptPreview` in state);
      `Tab::Prompts` footer nav; `on_route` seeds the current selection.
- [x] Relabelled the agent drawer's **"System prompt" → "Identity / persona"** with a
      "layer 2 of 4 — see Prompts" hint. 122 tests; e2e-verified (layers compose; the
      run-kind chip swaps the role preamble live).
- _Note:_ the brief shows `— none —` until the workspace dir is first scaffolded by a real
  turn (scaffold runs on a run, not at boot) — accurate, but a future tweak could scaffold
  on workspace create so the brief is visible immediately.

**E-2 — Memory panel (view / edit / clear / pause)** _(done)_
- [x] The Prompts view's "Working memory" card now shows a size/marker line + paused badge,
      an **editable** summary (Save overwrites it, keeping the marker), **Clear** (reset summary
      + marker → re-windows), and **Pause/Resume** (freeze auto-summary).
- [x] `agent_memory.paused` column (schema + idempotent `add_agent_memory_paused_column`
      migration); `db::agent_memory::set_paused`; commands `SetMemory`/`ClearMemory`/
      `SetMemoryPaused`; `maybe_summarize` skips a paused agent; a summarizer write preserves
      the operator's pause. 123 tests; e2e-verified (save → persisted, pause → `paused=1`).

**E-3 — Settings store + configurable defaults** _(adds a `settings` kv table)_
- [ ] Additive `settings(key TEXT PRIMARY KEY, value TEXT)` + `db::settings` (get/set/all).
- [ ] Role preambles editable as **global defaults** (3 keys); run sites read the override with
      the const as fallback. Memory knobs editable too: `MEMORY_WINDOW`, `SUMMARIZE_FRACTION`,
      `DEFAULT_CONTEXT_TOKENS`, `SUMMARIZE_AT`.
- [ ] A **Settings** view (or a section on `/prompts`) to edit them; "reset to default" per key.

**Gate every increment:** `cargo fmt --check` · `clippy -D warnings` · `cargo test`.

---

## Decisions

- [x] **Inspector placement** — a dedicated `/prompts` footer-nav view (not buried in the agent
      drawer), since the preview spans agent × run-type × workspace.
- [ ] **Override scope (resolve before E-3)** — role preambles + thresholds as **global only**,
      **global + per-agent override**, or **global + per-workspace override**. *Recommendation:*
      ship **global defaults** in E-3 (biggest win, simplest), add per-agent override later if
      needed (persona is already per-agent, so the seam exists).

## Out of scope (later)

- Per-run "exactly what was sent" capture (log the literal assembled prompt + tokens per turn).
- Editing the `notify` / `summarize` instruction prompts (fold into E-3 settings if wanted).
- Live token-count display in the inspector (needs a tokenizer; the byte/char estimate is enough).
