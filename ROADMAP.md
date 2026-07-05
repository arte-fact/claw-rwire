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

## Elegance constraints (bind every phase)

1. **Derive, don't store.** The filesystem and git are already the registry — a project IS
   its directory, its kind IS a pure function over its files, its history IS
   `request_repos`. Rows exist only for state with an independent lifecycle. Net new
   storage across all phases: **one column** (`task_runs.checks`), **zero tables**.
2. **One runner, three consumers.** Harness CI, the agents' `checks` tool, and the
   delivery gate share one implementation (`src/ci.rs`); a machine verdict means the same
   thing everywhere or it means nothing.
3. **Complexity flows upstream.** Anything reusable by any rwire app (base-path, proxy
   plumbing if generic) lands in the framework; claw stays a consumer. The
   Breadcrumb/Progress precedent, as policy.
4. **Reuse the seams that exist**: teardown hooks (P0 kills), the boot-recovery pattern
   (P2 staging), `disclosure_row` (check tails), sparse settings (overrides only when
   overridden), the detail/projection pattern (project page = request page's shape).
5. **New-file budget**: `kinds.rs`, `ci.rs`, `bridge/preview.rs`, `ui/project_page.rs` —
   four modules, each under the house 300-line cap. A phase that wants a fifth file
   justifies it in its commit message.

## P0 — isola on (the safety floor for everything below)

> **Shipped 2026-07-05** (claw + 3 upstream isola fixes). The plan's one deviation, for
> the better: the lost `feat/exec-command-captured` isola lib branch was NOT rebuilt —
> integration is **CLI-driven** (`isola create/status/exec/destroy` subprocesses; ~15 ms
> warm exec, exit codes propagate) — so the `sandbox` cargo feature dissolved into runtime
> detection (`isola` on PATH). One build, no feature matrix.

- [x] **Enable**: `CLAW_SANDBOX=isola` in `.env`; runtime-detected binary (no cargo
      feature). Boot log prints `sandbox: isola mode`. CLAUDE.md documents the setup.
- [x] **Lane mounts + naming**: `sandbox::name_for` — `claw-<ws>` canonical,
      `claw-<ws>-<req8>` per lane, mount = the lane root; sandboxes created on demand at
      first `bash` (`--no-cache --env rust,git,nodejs` — minimal, self-contained, never
      the operator's ssh-keys/claude-config/git-config; ~3 min once per lane, billed to
      the first command). Unit-tested naming; live integration test green.
- [x] **Checks route through ExecStrategy**: `exec::run_in(strategy, cmd, timeout)`;
      `launch_validator`'s machine checks now sandbox with the lane.
- [x] **Teardown kills processes** (harvested L5): deliver/abandon destroy the lane's
      sandbox before removing the tree (admin workspace-delete already did); whatever a
      run left behind dies with the rootfs.
- [x] **Escape regression**: gated integration test (`cargo test -- --ignored isola_`,
      auto-creates its sandbox) — workspace write-back visible, `$HOME` write does not
      escape, absolute host paths invisible, in-sandbox `timeout` reaps. Green 2026-07-05
      (187 s incl. fresh provision).
- [x] **Host allowlist documented** (below).
- [x] **Provisioning is announced, not silent** (drift): a fresh lane's sandbox warms on
      a background thread at task launch (overlapping the first LLM turn; ensure()
      serializes per name so the racing first bash waits out the remainder, not the full
      build) and the request's origin chat gets a 🧰 one-liner — a 3-minute first bash
      must never read as a hang.
- [x] **Isola cache reworked upstream** (operator-directed, 2026-07-05): the layered
      find-newer diff cache was retired outright (−655 lines; unfixable by patching — it
      shipped base layers without /var/lib/dpkg or the usr-merge links) in favour of the
      snapshot cache as the ONE path, self-healing (sanity-check on extract; a bad
      tarball is discarded and the create falls through to full provisioning), with the
      capture moved to /var/tmp — the namespace tmpfs over /tmp had been silently
      evaporating every capture. Lane sandbox creation: **3 min → 22 s** (first-ever
      env-combo create still pays one full provision). claw dropped --no-cache; the
      integration test runs in 25 s incl. creation. isola: `f7586ff`, `31cf2da`.
      Still open there: a neutral git identity for agent sandboxes (host identity is
      inherited).

### Host allowlist (what deliberately stays host-side)

- **Harness git** (sweep, branches, worktrees, delivery merges): claw's own code with a
  hermetic env — sandboxing it would need the data dir mounted, defeating the boundary.
- **The DB + server**: claw itself is the trust anchor.
- **File tools** (`read`/`write`/`edit`/`list`/`find`): host-side behind the workspace
  jail (lexical + canonicalization layers, symlink-guarded) — they never execute content.
- **Web/MCP tools**: network egress by design; no filesystem writes outside the jail.
- Everything an agent *executes* (`bash`, machine checks, P1 CI, P2 previews) goes
  through `ExecStrategy` and sandboxes with the lane.

## P1 — Projects become first-class (the unit CI/CD binds to)

The Request promotion, one level down: everything P1b–P3 needs is per-project, but a
project today is a directory with no identity. **Kind is the paved-road bundle**: it binds
the CI recipe, the preview recipe + base-path knob, the prompt layer, the tool profile,
and the skills — one concept rounding up prompts/tools/skills instead of settings sprawl.

- [ ] **Projects are derived, never registered**: identity = the directory `repos()`
      already lists; **kind = a pure function** over its files (rwire dep in Cargo.toml →
      `rwire-app`; Cargo.toml → `rust-bin`; package.json → `node-app`; else `other`),
      computed on read — no table, no registration hooks, no sync bugs when a directory
      moves. The only stored state is the sparse override
      (`settings: project.<ws>.<name>.kind|checks|preview`), written only when set.
      *Site*: new `src/kinds.rs` — one enum + pure fns (`detect`, `checks_recipe`,
      `preview_command`, `prompt_layer`, `skill_source`) + tests; nothing else.
- [ ] **Prompt composition grows the project layer**: role → workspace (AGENT.md) →
      project (kind conventions + contracts: "these exact checks will judge your pass —
      run them first"; "serve under $CLAW_PREVIEW_BASE") → task spec. Producers
      pre-empting CI is the cheapest review round.
      *Site*: `run_system_prompt` composition, `scheduler/mod.rs` launches (the task's
      touched/target project resolved from the spec's `projects/<name>` paths or the
      request's `request_repos`).
- [ ] **Project page + strip**: `/w/:wid/projects/:name` — develop CI status, staging /
      preview links, request history (from `request_repos`), kind + overrides settings;
      Projects strip on the Work tab beside Requests; breadcrumb-integrated (workspace ›
      project). *Site*: `ui/project_page.rs`, `workspace_tasks/mod.rs`.

## P1b — CI: machines judge before models (completes L1/M2)

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
- [ ] **`checks` tool**: producers/validators run the project's own CI suite on demand —
      the same runner the harness uses, one source of truth for machine judgement.
      *Site*: `tools/` new tool over `src/ci.rs`, in producer + validator profiles.
- [ ] Ops (harvested L1): put Critic on a stronger model than the producers — two fields
      on the System page; do it when picking the next model lineup.

## P2 — CD: every branch a URL (the request model completes)

- [ ] **Preview supervisor — in-memory, no table**: previews are process state and
      processes die with the server, so the supervisor is a bridge-owned map
      (`(workspace, repo) → {port, child, kind}`), ports from `CLAW_PREVIEW_PORTS`
      in `.env`, health = TCP/HTTP probe with a deadline. Stop on deliver/abandon (the
      tree-teardown seam); boot recovery = the map starts empty and staging relaunches
      lazily on first hit (the macro-review recovery pattern, simpler: nothing to scan).
      Start = run the project's kind command **from the request's tree** under isola,
      reusing the P1b build artifact.
      *Site*: new `src/bridge/preview.rs` only; hooks in `requests/{mod,control}.rs`.
- [ ] **How a preview knows what to run**: the project's kind supplies the command
      (rwire-app: `cargo run --release`; node-app: `npm start`; override on the project
      row), with `CLAW_BIND=127.0.0.1:<port>` + `CLAW_PREVIEW_BASE` injected via env.
- [ ] **`preview` tool**: a producer starts/restarts its project's preview mid-task and
      gets the URI back — the debug loop as a tool call, not an improvised bash server.
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

## P3 — rwire specialization: the flagship kind

- [ ] **Jailed `clone` tool** (harvested G0.5 — the real workflow arrived): a dedicated
      tool cloning a URL or local path into `projects/<name>` with the hermetic git env,
      depth/pin options, symlink-guarded destination. Feeds the template below and ends
      the "agents improvise clones via bash" era.
      *Site*: `tools/repo.rs` (alongside repo_status/log/diff), concierge + producer
      toolboxes.
- [ ] **Workspace template "rwire app"** = create workspace + register an `rwire-app`
      kind project (P1 machinery); instantiation = scaffold skeleton (`Cargo.toml` with the framework as a **pinned
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
