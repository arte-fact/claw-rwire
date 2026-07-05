# Platform: isolation, CI, CD, and the rwire thesis

> **The active roadmap** (2026-07-05). Predecessors — `requests.md` (G0–G4) and
> `levers.md` (L1–L6, M2–M3) — are complete historical records; their open boxes are
> harvested here. The four axes are one system, in dependency order: **isola** makes
> running agent-built code safe; **CI** makes it judged by machines before models; **CD**
> makes every request branch a clickable preview; **rwire specialization** is the product
> thesis that makes the other three simple (an rwire app is one self-contained binary on
> one port — the ideal unit to build, check, and preview). Cadence: each phase gate-green,
> live-verified, ticked with a commit.

## Findings (verified 2026-07-05)

- **isola is plumbed, not enabled** (R6): `ExecStrategy::{Host,Isola}` in `tools/exec.rs`,
  per-workspace sandboxes, bind mounts, sandbox-side timeout reaping, explicit refusal to
  silently fall back to host — behind the `sandbox` cargo feature + `CLAW_SANDBOX=isola`.
  Live binary: feature **off**, env **unset**; the `isola` binary IS installed
  (`~/.cargo/bin/isola`). Agent bash therefore runs on the host (cwd-pinned, hermetic env).
- **CI in embryo**: the L1 checks command — one string per workspace
  (`settings: workspace.<id>.checks`), run host-side at review launch
  (`scheduler/mod.rs launch_validator`), output as prompt prose. Workspace-global (wrong
  repo once work moves), and its result isn't a judgement the system can act on.
- **CD as improvisation**: the Empire preview exists because a request asked "start the
  server on port 7778". No owner, no lifecycle. The request model has the seams ready:
  branch per request, tree per request, deliver/abandon teardown.
- **rwire specialization already happening**: agents cloned the framework (sovereign clone
  flow proven), built Empire on it; operator promotes gaps upstream (Breadcrumb, Progress,
  tokens precedent). Missing: the paved road.

## P0 — isola on (the safety floor for everything below)

- [ ] **Enable**: release built `--features sandbox`; `CLAW_SANDBOX=isola` in `.env`.
      *Site*: build script/ops + `src/sandbox.rs` (`SandboxMode::resolve`).
      *Acceptance*: boot log prints `sandbox: isola mode`; a task's `bash` transcript
      shows sandbox execution; `cargo build --release` docs updated (CLAUDE.md gains the
      feature flag note).
- [ ] **Lane mounts audit**: G4 tasks run in `trees/<ws>/<req-short>`, not the canonical
      workspace — the strategy's `mount` must be the **lane root** (`task_root`) and the
      sandbox name must not collide across lanes of one workspace.
      *Site*: wherever `ExecStrategy` is constructed (`bridge/mod.rs build_toolbox` →
      `CoderToolbox`); audit `src/sandbox.rs` lifecycle (who creates/destroys sandboxes;
      per-workspace vs per-lane identity).
      *Acceptance*: real-git + sandbox test — a request-lane bash writes in its tree,
      cannot read the canonical workspace or another lane.
- [ ] **Checks/CI route through ExecStrategy**: `launch_validator`'s machine-check call
      uses `exec::run` directly (host, always) — switch to the task's strategy; same for
      P1 runners.
      *Site*: `scheduler/mod.rs` (checks call), `tools/exec.rs` (expose a strategy-aware
      entry with caller-chosen timeout).
- [ ] **Teardown kills processes** (harvested L5): abandon/stop/restart tears the lane's
      sandbox down, killing everything in it — closes the orphan-writer resurrection
      under `trees/` (the process-group kill we deferred, now free via the sandbox).
      *Site*: `requests/control.rs abandon`, `bridge/tasks.rs stop/restart`,
      `gitops::remove_request_tree` call sites.
      *Acceptance*: abandon mid-`cargo build` leaves no surviving child; `trees/` stays
      deleted.
- [ ] **Escape regression**: the symlink episode's probe as a test — sandboxed bash
      reading through `projects/<link>` → outside fails; feature-gated
      (`#[cfg(feature = "sandbox")]`), run in the P0 live-verify.
- [ ] **Host allowlist documented**: what deliberately stays host-side (harness git, DB,
      the server itself) and why — one section in this doc, referenced from CLAUDE.md.

## P1 — CI: machines judge before models (completes L1/M2)

- [ ] **Per-repo detection replaces the global command**: after each successful producer
      pass, for every repo the pass touched (from `pass_commits`), a `Cargo.toml` at the
      repo root ⇒ run `cargo fmt --check`, `clippy --all-targets -- -D warnings`,
      `cargo test` (+ `build --release` only where a `[[bin]]` exists and P2 needs the
      artifact). The `workspace.<id>.checks` setting becomes a **per-repo override**
      (`workspace.<id>.checks.<repo>`), kept as the escape hatch for non-cargo stacks.
      *Site*: new `src/ci.rs` (detection + runner + result types); called from
      `scheduler/mod.rs finished()` after `commit_pass`.
- [ ] **Fail fast, review green**: red checks after a producer pass short-circuit to a
      producer retry with the failure tail as feedback — no Critic round spent on what
      clippy already knew. *Decision (recommended)*: check-retries are their own bounded
      counter (2, like verdict-less retries), NOT `max_iterations` — reviewer budget and
      machine budget stay separate; exhausted check-retries fall through to the normal
      review (Critic sees the red tail and judges).
      *Site*: `finished()` producer arm before `gate_action`; pure decision fn + tests.
- [ ] **Structured results**: `task_runs.checks` column (JSON:
      `[{repo, check, ok, tail}]`, migration duplicate-tolerant like `commits`); rendered
      as a badge row in the run entry (task page) and aggregated on the request page/card
      (`✓ checks` / `✗ fmt, test`).
      *Site*: `db/task_runs.rs`, `bridge/tasks.rs run_view`, `ui/run_chain.rs`,
      `requests/view.rs`.
- [ ] **The macro gate sees CI**: auto-delivery requires the request's latest checks
      green; the Deliver button shows red state (human override allowed, recorded in
      `validation`).
      *Site*: `requests/control.rs on_validated/deliver_now`, `ui/request_page.rs`.
- [ ] **CI runtime tier**: 600s timeout const, per-lane `CARGO_TARGET_DIR` reuse
      (`<tree>/.claw-target` shared across the request's passes) so iteration builds are
      incremental.
      *Site*: `src/ci.rs`; verify tree teardown removes it.
- [ ] Ops (harvested L1): put Critic on a stronger model than the producers — two fields
      on the System page; do it when picking the next model lineup.

## P2 — CD: every branch a URL (the request model completes)

- [ ] **Preview supervisor**: `previews` table (`request_id`, `repo`, `port`, `pid`,
      `status`, `started_at`) + a bridge module owning start/health/stop. Start = build
      (reuse P1 artifact) then run the binary **from the request's tree** under isola,
      port from a pool (`CLAW_PREVIEW_PORTS=7800-7899` in `.env`), health = TCP/HTTP
      probe with a deadline. Stop on deliver/abandon (same seam as tree teardown) and on
      boot for rows whose pid is gone (recovery, like macro reviews).
      *Site*: new `src/bridge/preview.rs`, `db/schema.sql`, hooks in
      `requests/{mod,control}.rs`.
- [ ] **How a preview knows what to run** — *decision (recommended)*: a `Preview` line in
      the project's `AGENT.md`-style metadata is too loose; use a per-repo setting
      (`workspace.<id>.preview.<repo>` = command, default for rwire apps in P3:
      `cargo run --release`) with `CLAW_BIND=127.0.0.1:<port>` injected via env.
- [ ] **Routing — DECIDED (operator, 2026-07-05): integrated dynamic URIs.** The claw
      server proxies `/preview/<req-short>/…` (and `/staging/<project>/…`) to the pooled
      ports itself — same origin, so previews inherit claw's **auth gate** (agent-written
      servers never face the internet raw), no nginx writes, no DNS/TLS work, and one URI
      vocabulary shared by the UI, the chat, and the agents.
      *Site*: a proxy route in the rwire server layer (or claw-side handler) streaming to
      `127.0.0.1:<port>`; websocket pass-through included (rwire previews ARE websockets).
- [ ] **rwire base-path support** (the prerequisite we own): `Server::base_path()` —
      wire endpoint, asset URLs, and `Link::route` become prefix-aware, so every rwire
      app is mountable under a dynamic URI natively. Ships upstream (framework), adopted
      by the preview supervisor via env (`CLAW_BASE_PATH=/preview/<short>`).
      **The base-path contract**: the supervisor always exports the mount point
      (`CLAW_PREVIEW_BASE=/preview/<short>`) alongside the port; rwire honors it natively
      (base_path upstream), and most other stacks map it to their own knob (Vite `base`,
      Next `basePath`, CRA `PUBLIC_URL`, …) — the template/STANDARDS instruct agents to
      wire it, so honoring the env var is reviewable work, not infrastructure. Truly
      path-hostile apps fall back to `localhost:<port>` for agent access.
- [ ] **Agents propose and visit preview links**: the preview URI is injected into the
      producer/validator prompts for the request (debug loop: start preview → fetch the
      URI → see the real failure → fix); a jailed same-origin `visit` tool (GET, capped
      body) lets reviewers exercise the running branch; chat receipts + request page
      render the link. *Endgame (P3+)*: rwire's wire protocol is machine-readable — a
      future tool drives the preview's UI structurally over the websocket, no browser.
- [ ] **Develop staging**: one long-lived preview per web project tracking `develop`,
      restarted on delivery (hook in `deliver()` after merge). Named
      `<project>.staging.…` under the same routing.
- [ ] **Surfaces**: Preview link on the request page header (status-aware: validating →
      "preview this branch"); the **macro-review prompt gets the URL** — the reviewer
      (agent with `fetch`-like tool? at minimum the human) exercises the running branch
      before Deliver; preview state shown on the request card.
      *Site*: `ui/request_page.rs`, `requests/control.rs launch_validation` brief.
- [ ] Backlog (harvested L5, matters more once CD auto-restarts staging on delivery):
      working-tree-less delivery merge (`merge-tree` + `commit-tree` + `update-ref`) —
      closes the delivery-vs-dirty-canonical race for good.

## P3 — rwire specialization: the paved road

- [ ] **Jailed `clone` tool** (harvested G0.5 — the real workflow arrived): a dedicated
      tool cloning a URL or local path into `projects/<name>` with the hermetic git env,
      depth/pin options, symlink-guarded destination. Feeds the template below and ends
      the "agents improvise clones via bash" era.
      *Site*: `tools/repo.rs` (alongside repo_status/log/diff), concierge + producer
      toolboxes.
- [ ] **Workspace template "rwire app"**: dashboard create gains a template select;
      instantiation = scaffold skeleton (`Cargo.toml` with the framework as a **pinned
      sovereign clone** at `projects/rwire` + path dep, `src/main.rs` hello-app),
      `STANDARDS.md` rwire section (St tokens, components-first, no raw attrs, no
      hand-rolled chips), CI defaults on, preview command preset.
      *Site*: `bridge/seed.rs`-style template module + dashboard UI.
- [ ] **Skill seeding**: copy the framework clone's `SKILL.md` into `skills/rwire.md` at
      instantiation and on clone re-pin — agents read the guide the operator maintains;
      the docs-manifest (L3) already advertises it every turn.
- [ ] **The dogfood loop, explicit**: template `docs/rwire-gaps.md`; validator standards
      say "framework friction goes in the gaps doc, never worked around by re-styling";
      operator triages gaps upstream (Breadcrumb/Progress precedent), re-pins the clone.
- [ ] **Success metric (the P3 live-verify)**: a fresh "build me an X" ask in an
      rwire-template workspace ships a previewable app with **zero operator touches**
      between plan approval and Deliver — plan gate and Deliver click stay human by
      design; everything between is agents + machines.

## Backlog (harvested, scheduled by pull)

- Working-tree-less delivery merge (see P2 last box).
- Background error surfacing (L6): scheduler/gitops errors reach a visible surface
  (Activity feed or the toast queue), not just the log.
- Error taxonomy (L6): user-actionable vs telemetry, so toasts don't train the operator
  to dismiss.
- Checks-command scoping nit dissolves into P1 per-repo detection.
