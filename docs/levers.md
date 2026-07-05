# Levers: workflow improvements from the Empire e2e (2026-07-04)

> Born from the from-scratch end-to-end (docs/requests.md complete, the Empire game
> delivered): every lever below is a friction we *watched* cost wall-clock, tokens, or
> review quality. Ranked by impact; each phase ends gate-green, ticked with a commit.

### L1 — review economics (the biggest leak)
Critic twice burned its entire round budget inspecting without concluding; mechanical
nits consumed full produce⇄review cycles.
- [x] **No-verdict reviews don't consume iterations** (744d23c): a verdict-less validator run is a
      review *retry* (bounded, then falls back to today's revise), not a revise — the
      producer's budget stops paying for the reviewer's indecision
- [x] **Conclude-now nudge** (744d23c): a retried review is told its previous turn ended without
      `verdict` and must conclude
- [x] **Machine checks before model review**: a per-workspace check command (Settings),
      run by the harness after each successful producer pass, its capped output injected
      into the validator prompt — lint/build/test verdicts come from machines, judgment
      from the reviewer
- [ ] Ops (manual): put Critic on a stronger model than the producers — the resolution
      system already supports per-agent models; this is configuration, not code

### L2 — structure has no gate
The Empire game shipped as one 3,386-line `main.rs`; nothing objected.
- [x] Seed `STANDARDS.md` at workspace birth with structure rules (modules per
      subsystem, file-size soft caps, tests beside code) — validators already receive it
- [x] Update the live studio workspace's memo to match (+ studio checks command set to `cargo test` in projects/empire)

### L3 — knowledge doesn't flow
The concierge re-read `empire.bas` five times per turn; the second ask re-planned cold.
- [x] `board_status` lists each workspace's `docs/` files — the concierge sees what
      reference material exists without re-reading it
- [x] Delivery writes a one-line receipt into the owner's workspace memory — future
      plans start from "what has shipped here"

### L4 — trouble is quieter than success
M3 sat at five failing tests mid-iteration with no signal until it recovered.
- [x] Revise rounds past the first ping the request's origin chat with the round count
      and the reviewer's headline feedback

### L5 — mechanical debts (later)
- [ ] Working-tree-less delivery merge (`merge-tree` + `commit-tree` + `update-ref`) —
      closes the delivery-vs-dirty-canonical race
- [ ] Abandon kills in-flight workers (process-group kill) — closes the orphan-writer
      resurrection under `trees/`

### L6 — error-handling audit (2026-07-04, from the failed-Deliver incident)
The app HAS an error channel (`App.error` via `surface()`) — but only chat + Files rendered
it, and only `bridge/{files,tasks}` wrote to it. The whole request control plane (Deliver /
Rollback / Push / Abandon / Restore) reported failures to the server log and a chat ping
only — invisible on the page the operator actually clicked. rwire ships a `Toast` component
the app never used.
- [x] Global error banner in the shell: `surface()`d failures now render on every page,
      dismissable (single-slot, last wins)
- [x] Request verbs surface their failures (delivery merge errors, rollback/push/abandon
      trouble, per-file restore) — plus chat ping + log as before
- [x] Sweep re-asserts critical project ignore lines (`node_modules/`, `target/`, `.env`)
      every pass — the incident's root: a React scaffold clobbered `.gitignore`, `add -A`
      tracked node_modules, and the git index tore (`unknown index entry format`)
- [x] Toast queue (rwire `Toast`): surface()d failures stack bottom-right on every page, capped at 5, per-toast dismiss; App.error stays the admin forms inline channel. Later:
      scheduler/gitops background errors surfaced somewhere visible (Activity feed shows
      lifecycle, not errors); error taxonomy (user-actionable vs telemetry)

### M2 — reject calibration (fixed 2026-07-05)
- [x] An agent validator's `reject` routes as a **severe revise** (bounded by the loop,
      feedback prefixed with its severity); only a human's reject — direct or at the
      exhaust gate — ends a task. Live cost it closes: two terminal rejects over fixable
      nits, each cancelling dependents and demanding a manual restart.
- [x] Validator prompt spells out the consequences (reject ends the task AND cancels
      dependents; fixable flaws are ALWAYS revise; when in doubt, revise)
