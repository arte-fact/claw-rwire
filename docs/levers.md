# Levers: workflow improvements from the Empire e2e (2026-07-04)

> Born from the from-scratch end-to-end (docs/requests.md complete, the Empire game
> delivered): every lever below is a friction we *watched* cost wall-clock, tokens, or
> review quality. Ranked by impact; each phase ends gate-green, ticked with a commit.

### L1 — review economics (the biggest leak)
Critic twice burned its entire round budget inspecting without concluding; mechanical
nits consumed full produce⇄review cycles.
- [ ] **No-verdict reviews don't consume iterations**: a verdict-less validator run is a
      review *retry* (bounded, then falls back to today's revise), not a revise — the
      producer's budget stops paying for the reviewer's indecision
- [ ] **Conclude-now nudge**: a retried review is told its previous turn ended without
      `verdict` and must conclude
- [ ] **Machine checks before model review**: a per-workspace check command (Settings),
      run by the harness after each successful producer pass, its capped output injected
      into the validator prompt — lint/build/test verdicts come from machines, judgment
      from the reviewer
- [ ] Ops (manual): put Critic on a stronger model than the producers — the resolution
      system already supports per-agent models; this is configuration, not code

### L2 — structure has no gate
The Empire game shipped as one 3,386-line `main.rs`; nothing objected.
- [ ] Seed `STANDARDS.md` at workspace birth with structure rules (modules per
      subsystem, file-size soft caps, tests beside code) — validators already receive it
- [ ] Update the live studio workspace's memo to match

### L3 — knowledge doesn't flow
The concierge re-read `empire.bas` five times per turn; the second ask re-planned cold.
- [ ] `board_status` lists each workspace's `docs/` files — the concierge sees what
      reference material exists without re-reading it
- [ ] Delivery writes a one-line receipt into the owner's workspace memory — future
      plans start from "what has shipped here"

### L4 — trouble is quieter than success
M3 sat at five failing tests mid-iteration with no signal until it recovered.
- [ ] Revise rounds past the first ping the request's origin chat with the round count
      and the reviewer's headline feedback

### L5 — mechanical debts (later)
- [ ] Working-tree-less delivery merge (`merge-tree` + `commit-tree` + `update-ref`) —
      closes the delivery-vs-dirty-canonical race
- [ ] Abandon kills in-flight workers (process-group kill) — closes the orphan-writer
      resurrection under `trees/`
