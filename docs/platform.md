# Platform: isolation, CI, CD, and the rwire thesis

> Drafted 2026-07-05 from four operator axes. These are one system, in dependency order:
> **isola** makes running agent-built code safe; **CI** makes it judged by machines before
> models; **CD** makes every request branch a clickable preview; **rwire specialization**
> is the product thesis that makes the other three simple (an rwire app is one
> self-contained binary on one port — the ideal unit to build, check, and preview).
> Cadence as always: each phase gate-green, live-verified, ticked with a commit.

## Findings (what exists today)

- **isola is plumbed, not enabled** (R6): `ExecStrategy::{Host,Isola}` with per-workspace
  sandboxes, bind-mounted workspace dirs, sandbox-side timeout reaping, and an explicit
  refusal to silently fall back to host — behind the `sandbox` cargo feature and
  `CLAW_SANDBOX=isola`. The live binary is built **without** the feature and the env is
  unset, so every agent `bash` runs on the host (cwd-pinned, hermetic env — that's how the
  symlink episode's reads happened). The `isola` binary is installed on the box.
- **CI exists in embryo**: the L1 machine-check command (one string per workspace, run at
  review launch, output into the reviewer's prompt). It's workspace-global — it points at
  `projects/empire` while the work moved to `empire-rwire` — and its result is prose in a
  prompt, not a judgement the system can act on.
- **CD exists as improvisation**: the Empire preview runs because a request literally asked
  "start the server on port 7778". Nothing owns its lifecycle; the request model already
  has the perfect seams (branch per request, tree per request, deliver/abandon teardown).
- **rwire specialization is already happening organically**: the agents cloned the
  framework (now a sovereign clone), built Empire on it, and the operator promotes gaps
  upstream (Breadcrumb, Progress, tokens). What's missing is making this the *paved road*.

## P0 — isola on (the safety floor for everything below)

- [ ] Build the release with `--features sandbox`; set `CLAW_SANDBOX=isola`; boot log
      confirms the mode
- [ ] Verify sandbox lifecycle end-to-end: per-workspace sandbox creation, **lane trees as
      mounts** (G4 tasks run in `trees/<ws>/<req>` — the mount must be the lane root, not
      the canonical workspace), teardown with the tree
- [ ] Machine checks + future CI runners route through `ExecStrategy` (today they call
      `exec::run` directly — host, always)
- [ ] Regression: a sandboxed `bash` cannot read outside its mount (the symlink episode's
      probe, now expected to fail)
- [ ] Decide + document the host-exec allowlist that remains (harness git stays host-side
      by design)

## P1 — CI: machines judge before models (extends L1)

- [ ] Per-repo auto-detection replaces the workspace-global command: a touched repo with a
      `Cargo.toml` gets `cargo fmt --check` + `clippy -D warnings` + `cargo test` (+
      `build` for bins) in the lane tree; the manual checks command survives as a per-repo
      override/escape hatch
- [ ] **Fail fast, review green**: a red check after a producer pass auto-revises with the
      failure as feedback — a Critic round is never spent discovering what clippy already
      knew (the M2/L1 philosophy completed)
- [ ] Structured results on the run row (pass/fail per check + capped tail), not just
      prompt prose: check badges on the task page, aggregate on the request page/card
- [ ] The macro gate sees CI: auto-delivery requires green; the Deliver button shows red
      state (the human can still override)
- [ ] CI tier timeout (builds ≫ the 180s checks cap) + per-lane target-dir reuse so
      iteration passes rebuild incrementally

## P2 — CD: every branch a URL (the request model completes)

- [ ] Preview supervisor: build + run a project's server **from the request's tree** on a
      pooled port; health-checked; lifecycle tied to the request (start on demand or at
      `validating`; killed at deliver/abandon alongside the tree teardown)
- [ ] `develop` staging: one long-lived preview per web project, restarted on delivery —
      the "production-ish" mirror of the integration branch
- [ ] Reverse-proxy routing — decision box: wildcard subdomain
      (`<req-short>.preview.raptors.pizza`, needs DNS+TLS wildcard) vs claw-managed nginx
      include + reload vs a built-in proxy route in the claw server. Constraint: rwire
      apps assume a root path, so subdomain/port routing beats path prefixes
- [ ] The request page grows a **Preview** link (and the macro-review prompt gets the URL:
      the reviewer — agent or human — clicks the actual running branch before Deliver)
- [ ] Previews run under isola (P0) — an agent-built server never runs raw on the host

## P3 — rwire specialization: the paved road

- [ ] Workspace template "rwire app": scaffolded project skeleton, the framework as a
      pinned sovereign clone (the flow the live instance already proved), `STANDARDS.md`
      with rwire conventions (St tokens, components-first, no raw attrs), CI defaults on
- [ ] The rwire skill seeded into `skills/` from the framework's own SKILL.md — agents
      read the same guide the operator does; refreshed when the clone updates
- [ ] CD defaults tuned to rwire's shape: one binary, one port, `CLAW_BIND`-style config —
      preview = `cargo run --release` in the tree, nothing more
- [ ] The dogfood loop made explicit: framework gaps found by agents land as
      `docs/rwire-gaps.md` entries in the workspace; the operator promotes upstream
      (Breadcrumb/Progress precedent) and the clone re-pins
- [ ] Success metric: a fresh "build me an X" ask in an rwire-template workspace ships a
      previewable app with zero operator intervention between plan approval and Deliver
