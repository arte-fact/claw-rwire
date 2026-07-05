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

## P1 — Projects become first-class (the unit CI/CD binds to) — SHIPPED 2026-07-05

The Request promotion, one level down: everything P1b–P3 needs is per-project, but a
project today is a directory with no identity. **Kind is the paved-road bundle**: it binds
the CI recipe, the preview recipe + base-path knob, the prompt layer, the tool profile,
and the skills — one concept rounding up prompts/tools/skills instead of settings sprawl.

**Live-verified** (studio/empire, real instance): the project page renders the
breadcrumb (Studio › empire), the derived kind (`rust-bin`, correctly detected — Cargo.toml,
no rwire dep), the request-history changelog (10 requests, status-badged, client-routed), and
the Projects strip on the Work tab. Override round-trip proven: pinning `rwire-app` flips the
badge to `rwire-app · operator override`; clearing returns it to `rust-bin · detected`.
Commits `2951e36` (kinds + prompt layer + page), `ce40094` (strip).

- [x] **Projects are derived, never registered** (2026-07-05): identity = the directory `repos()`
      already lists; **kind = a pure function** over its files (rwire dep in Cargo.toml →
      `rwire-app`; Cargo.toml → `rust-bin`; package.json → `node-app`; else `other`),
      computed on read — no table, no registration hooks, no sync bugs when a directory
      moves. The only stored state is the sparse override
      (`settings: project.<ws>.<name>.kind|checks|preview`), written only when set.
      *Site*: new `src/kinds.rs` — one enum + pure fns (`detect`, `checks_recipe`,
      `preview_command`, `prompt_layer`, `skill_source`) + tests; nothing else.
- [x] **Prompt composition grows the project layer** (2026-07-05): role → workspace (AGENT.md) →
      project (kind conventions + contracts: "these exact checks will judge your pass —
      run them first"; "serve under $CLAW_PREVIEW_BASE") → task spec. Producers
      pre-empting CI is the cheapest review round.
      *Site*: `run_system_prompt` composition, `scheduler/mod.rs` launches (the task's
      touched/target project resolved from the spec's `projects/<name>` paths or the
      request's `request_repos`).
- [x] **Project page + strip** (2026-07-05): `/w/:wid/projects/:name` — develop CI status, staging /
      preview links, request history (from `request_repos`), kind + overrides settings;
      Projects strip on the Work tab beside Requests; breadcrumb-integrated (workspace ›
      project). *Site*: `ui/project_page.rs`, `workspace_tasks/mod.rs`.

## P1b — CI: machines judge before models (completes L1/M2) — SHIPPED 2026-07-05

Kind now binds the CI recipe (the P1 promise realised). Machine judgement moved from a single
workspace command run at review time to per-repo CI at producer-finish, with fail-fast on red.
Commits `b73b43b` (core), `860408f` (structured results), `b1542ca` (macro gate), `477701f`
(checks tool). **Live-verified**: a projects/ci-smoke smoke task ran Plan→Build→Review→delivered — the harness ran fmt/clippy/test in the isola lane, recorded task_runs.checks all-green, rendered the badge strip, and the green gate auto-delivered.

- [x] **Per-repo detection replaces the global command** (2026-07-05): after each successful
      producer pass, every repo the pass committed is judged by its **kind's** recipe
      (`ProjectKind::checks()` — Rust kinds ⇒ `cargo fmt --check`, `clippy --all-targets -D
      warnings`, `cargo test`; other stacks override-only). `workspace.<id>.checks` became the
      per-repo override (`…checks.<repo>`, meta key back-compat). `build --release` deferred to
      P2 (it needs the artifact; keeps iteration fast). *Site*: `src/ci.rs`, `kinds.rs`,
      `scheduler/mod.rs finished()`.
- [x] **Fail fast, review green** (2026-07-05): red checks re-run the producer with the
      failure tail (surfaced by `producer_thread`), bounded by its own counter
      (`MAX_CHECK_RETRIES = 2`, separate from `max_iterations`); past the cap it falls through
      to the normal review with the red result visible. `unreviewed_output` skips a red pass so
      a restart re-produces. Pure decision covered by a scheduler test.
- [x] **Structured results** (2026-07-05): `task_runs.checks` column (JSON
      `[{repo,check,ok,tail}]`, duplicate-tolerant migration); a badge strip per producer run
      (`run_chain::checks_row`, red tails behind a disclosure) and a Checks section on the
      request page (`request_checks` unions each task's latest pass).
- [x] **The macro gate sees CI** (2026-07-05): auto-delivery (`on_validated` pass) is held
      when the request's latest checks are red — pings the operator, records the hold in
      `validation`; the Deliver button becomes "Deliver anyway" (explicit human override,
      `deliver_now` ungated).
- [x] **CI runtime tier** (2026-07-05): 600s per-check timeout (`ci::CHECK_TIMEOUT`). No
      shared `CARGO_TARGET_DIR` — Rust gitignores `/target` and the lane worktree persists
      across passes, so builds are already incremental (and a shared dir would fight the
      sandbox mount). Simpler than the planned `<tree>/.claw-target`, same outcome.
- [x] **`checks` tool** (2026-07-05): `ChecksToolbox` runs the same per-repo runner on demand,
      composed into both the producer and validator toolboxes; a red result returns as a failed
      call. *Site*: `tools/checks.rs`.
- [ ] Ops (harvested L1): put Critic on a stronger model than the producers — two fields
      on the System page; do it when picking the next model lineup.

## P2 — CD: every branch a URL (the request model completes)

> **P2a shipped 2026-07-05** (`d608fec` supervisor+tool, `9dde87d` amended). One new file
> `bridge/preview.rs` + `tools/preview.rs`, in-memory table-free supervisor, teardown hooks in
> `deliver()`/`abandon()`/workspace-delete. Feasibility unlocks proven in exploration: isola
> shares the host netns (`namespace.rs:147` — no `CLONE_NEWNET`) so an in-sandbox server is
> reachable at `127.0.0.1:<port>` from the host; a long-lived preview = an `isola exec` held as a
> background `Child`, stopped by an in-sandbox `/proc/*/environ` sweep on a unique `CLAW_PREVIEW_ID`
> (the isola-exec host handle can't reap the namespaced server). **Verified**: an ignored
> integration test drives the whole loop on the host strategy (resolve override → spawn with the
> injected `CLAW_BIND` → health-probe → the port answers `preview ok` → stop reaps it and frees the
> port); the release binary is deployed live (isola mode, health green) with `preview` in every
> producer toolbox. Live *agent-UI* exercise deferred to P2b's `/preview/<short>/` URL — a plain
> GET surface, far more reliable than a WS composer-send (headless-driver flakiness, tracked).
>
> **P2b shipped 2026-07-05** (rwire `0bcea47`, claw `6ca455d`+`6ee06d9`; FIFO tie-break fix
> `db1073b` en route). The same-origin URL, landed as two upstream rwire primitives + thin claw
> wiring. **Proxy** (`ServerConfig::proxy(ProxyResolver)`): a path→`(port, prefix)` closure consulted
> *after* the auth gate — proxied upstreams inherit claw's login — that rewrites the request line to
> strip `/preview/<key>` and bidirectionally streams the raw `TcpStream` to `127.0.0.1:<port>`; a WS
> upgrade rides through untouched (the pump is byte-agnostic). **Base-path** (`CapsuleConfig::base_path`
> / `ServerWithRoot::base_path`): a *client-runtime* concern — an injected `const BASE` prefixes the
> capsule's WS URL + history while the client still speaks root-relative routes, so a prefix-stripping
> proxy means zero server-side path changes; PWA head suppressed under a base, injection charset-
> sanitized. Claw's `main.rs` registers `preview::resolve_proxy` (key→live port via the supervisor);
> the supervisor derives the mount base itself and injects `CLAW_PREVIEW_BASE`. **Verified**:
> deterministic all the way through — rwire integration test (a matched path forwards with prefix
> stripped, unmatched paths hit the app) + claw end-to-end test (real preview in the map → the actual
> resolver returns its port + prefix, unknown/non-preview → None) + base-path/rewrite unit tests; the
> release binary is deployed live with the proxy behind the auth gate (curl-confirmed: logged-out
> `/preview/*` → login form, unknown key → app fall-through, no hang). The end-user *browser*
> walkthrough of a real rwire preview waits on a previewable project (none registered live) and a
> reliable agent trigger — arriving with P2c develop-staging (auto-starts on delivery, no composer-send)
> and the tracked headless-driver fix.
>
> **P2c shipped 2026-07-05** (claw `29b2779`, tests `27a2493`). Previews become first-class across the
> loop. **Develop staging**: `refresh_staging` (re)starts a long-lived preview per touched web project
> on delivery — keyed `<ws>-staging-<name>`, in a background thread so a cold build never blocks the
> delivery worker; `start_for_lane`/`start_staging` share a core `start()`. **`visit` tool**: a jailed
> host-side GET to the branch's `127.0.0.1:<pool-port>` (capped body, CRLF/space-sanitized path), on the
> producer, validator, AND macro-review toolboxes. **Prompt injection**: `preview_hint` (gated to
> previewable lanes) feeds the producer prompt, the validator diff block, and the macro brief the
> same-origin URL + live status. **Surfaces**: a status-aware "Preview this branch ↗" header link (real
> new-tab nav — the preview is a separate app behind the proxy) and a green card pulse, both off
> `port_for`. **Verified**: deterministic — `visit` round-trip + no-preview + path-sanitize unit tests,
> `staging_short`/`preview_hint` unit tests, and the ignored end-to-end test extended to bring up
> develop staging on its own pooled port and route it via `resolve_proxy`; 326 tests green, deployed
> live. Still deferred (unchanged blocker): the human *browser* click-through of a live preview needs a
> registered web project (none live) — reachable now via a delivery→staging path once a web project
> exists — and the tracked headless-driver fix (`#65`).

- [x] **Preview supervisor — in-memory, no table**: bridge-owned `OnceLock<Mutex<HashMap>>` keyed
      `<ws>-<short>`, `{port, child, strategy, id}`, ports from `CLAW_PREVIEW_PORTS` (default
      7900-7920), health = TCP connect with a deadline. Stop on deliver/abandon (+ workspace
      delete); boot recovery = the map starts empty (staging relaunches lazily — P2c). Start runs
      the kind command from the request's lane under its exec strategy, reusing the P1b artifact.
- [x] **How a preview knows what to run**: `ProjectKind::preview_command()` (rwire-app: `cargo run`
      — debug, reusing the P1b build, not a from-scratch `--release`; node-app: `npm start`; else
      the per-project `project.<ws>.<name>.preview` override), with `CLAW_BIND=127.0.0.1:<port>` +
      `CLAW_PREVIEW_BASE` + `CLAW_PREVIEW_ID` injected via the command's env exports.
- [x] **`preview` tool**: composed into the producer toolbox; starts/restarts the branch preview
      (optional `project` arg picks one of several) and returns the URL — the debug loop as a tool
      call. `exec::spawn_in` added (long-lived, held `Child`, own process group on host).
- [x] **Routing — DECIDED (operator, 2026-07-05): integrated dynamic URIs.** The claw
      server proxies `/preview/<req-short>/…` (and `/staging/<project>/…`) to the pooled
      ports itself — same origin, so previews inherit claw's **auth gate** (agent-written
      servers never face the internet raw), no nginx writes, no DNS/TLS work, and one URI
      vocabulary shared by the UI, the chat, and the agents. Shipped as rwire
      `ServerConfig::proxy(ProxyResolver)` (resolver consulted after the auth gate; request
      line rewritten to strip the prefix; raw-`TcpStream` bidirectional pump — WS pass-through
      free); claw registers `preview::resolve_proxy`. Forward+strip covered by an rwire
      integration test, the claw resolver by an end-to-end test against a live supervisor entry.
- [x] **rwire base-path support** (the prerequisite we own): `CapsuleConfig::base_path()` /
      `ServerWithRoot::base_path()` — a **client-runtime** concern (thanks to the proxy's
      prefix-strip, server-side routing is untouched): an injected `const BASE` makes the
      capsule's WS URL + browser history prefix-aware while the client speaks root-relative
      routes. Ships upstream (framework); works proxied *and* direct (`localhost:<port>`), since
      an rwire app accepts its WS on any path. PWA head suppressed under a base; injection
      charset-sanitized against quote-breakout.
      **The base-path contract**: the supervisor always exports the mount point
      (`CLAW_PREVIEW_BASE=/preview/<short>`) alongside the port; rwire honors it natively
      (base_path upstream), and most other stacks map it to their own knob (Vite `base`,
      Next `basePath`, CRA `PUBLIC_URL`, …) — the template/STANDARDS instruct agents to
      wire it, so honoring the env var is reviewable work, not infrastructure. Truly
      path-hostile apps fall back to `localhost:<port>` for agent access.
- [x] **Agents propose and visit preview links**: `preview_hint` (gated to previewable
      lanes) injects the same-origin URI + live status into the producer prompt, the
      validator's diff block, and the macro-review brief (debug loop: start preview → fetch
      the URI → see the real failure → fix). A jailed same-origin `visit` tool (host-side
      GET to `127.0.0.1:<pool-port>`, capped body, CRLF/space-sanitized path) rides the
      producer, validator, AND macro-review toolboxes so reviewers exercise the running
      branch. *Endgame (P3+)*: rwire's wire protocol is machine-readable — a future tool
      drives the preview's UI structurally over the websocket, no browser.
- [x] **Develop staging**: one long-lived preview per touched web project tracking
      `develop`, keyed `<ws>-staging-<name>`, (re)started by `refresh_staging` after the
      merge in `deliver()` — in a **background thread** so a cold `cargo` build never blocks
      the delivery worker. `start_for_lane`/`start_staging` share a core `start()`; non-web
      repos are silently skipped. (Lazy-start-on-first-hit deferred: the proxy resolver is a
      sync hot-path with no workspace context — eager-on-delivery is the clean mechanism.)
- [x] **Surfaces**: a status-aware "Preview this branch ↗" link on the request page header
      (a real new-tab navigation — the preview is a separate app behind the proxy, not a
      claw route) when a preview is live; the macro-review brief carries the URL (the human
      or the `visit`-armed reviewer exercises the branch before Deliver); a green "preview"
      pulse on the request card. Both driven off `port_for`.
      *Site*: `ui/request_page.rs`, `ui/workspace_tasks/mod.rs`, `requests/control.rs`.
- [ ] Backlog (harvested L5, matters more once CD auto-restarts staging on delivery):
      working-tree-less delivery merge (`merge-tree` + `commit-tree` + `update-ref`) —
      closes the delivery-vs-dirty-canonical race for good.

## P3 — rwire specialization: the flagship kind

> **Template shipped 2026-07-05** (claw `af8c7de`; rwire published to GitHub `b887594`).
> **Operator decision — the git dep supersedes the sovereign clone:** the scaffolded app declares
> rwire as a **public GitHub git dep** (`rwire = { git = "https://github.com/arte-fact/rwire",
> branch = "main" }`) instead of a copied `projects/rwire` clone — an isola lane fetches it
> anonymously over https (no creds in the sandbox), and there is no clone to keep in sync (the
> stale-clone problem dissolves). `src/bridge/template.rs` (the justified 5th module, `WorkspaceTemplate`
> + `scaffold_rwire_app`) scaffolds a previewable `projects/hello` counter wiring the preview
> contract (`CLAW_BIND` + `CLAW_PREVIEW_BASE` → `.base_path`, committed on `develop`), the bundled
> rwire `skills/rwire.md` (`include_str!` from `../rwire`), rwire `STANDARDS.md` with the dogfood
> rule, and `docs/rwire-gaps.md`; kind/CI/preview follow automatically from the git-dep manifest
> (`RwireApp` detection matches a git-dep line). The dashboard create form gained a Blank / rwire-app
> `Select`. **Deterministically live-verified (no agent):** the picker scaffolded the tree, the isola
> lane fetched+compiled rwire from GitHub anonymously (`Compiling rwire … github.com/arte-fact/rwire?branch=main#b887594`),
> and the git-dep-built binary served with `const BASE='/preview/…'` + a BASE-prefixed WebSocket —
> the paved road end to end (the verify caught + fixed a skeleton compile bug before it could ship).

- [ ] **Jailed `clone` tool** — **DEFERRED** (operator, 2026-07-05): the git dep removes its rwire
      use case; add it when agents need to pull *other* repos. *Site (when built)*: `tools/repo.rs`.
- [x] **Workspace template "rwire app"** (2026-07-05): the dashboard picker creates the workspace and
      `scaffold_rwire_app` lays the paved road — `projects/hello` skeleton (git-dep `Cargo.toml` +
      base-path `main.rs`, committed on `develop`), rwire `STANDARDS.md`, CI + preview automatic from
      the kind. *Site*: `src/bridge/template.rs` + `ui/dashboard.rs` + `admin.rs`. `auto_approve` stays
      off — the plan gate is human by design.
- [x] **Skill seeding** (2026-07-05): the framework `SKILL.md` is bundled into claw at build time and
      written to `skills/rwire.md`; the AGENT.md brief + STANDARDS point agents at it. (Re-pin story
      simplified: with a git dep tracking `main`, updating the guide is a claw rebuild, not a clone
      re-pin. The L3 docs-manifest advertises `docs/` only, so surfacing is via the brief pointer.)
- [x] **The dogfood loop, explicit** (2026-07-05): the template ships `docs/rwire-gaps.md`; the rwire
      `STANDARDS.md` carries the rule ("framework friction goes in the gaps doc, never worked around
      by re-styling"). Operator triages gaps upstream (Breadcrumb/Progress precedent) and bumps the
      pinned rwire.
- [ ] **Success metric (the P3 live-verify)** — reachable now, awaits a real run: a fresh "build me an
      X" ask in an rwire-template workspace ships a previewable app with **zero operator touches**
      between plan approval and Deliver. The deterministic pieces (scaffold → git-dep build → base-path
      preview) are all green; this box needs one agent-driven request to Deliver.

## Backlog (harvested, scheduled by pull)

- Output-only requests confuse the macro review (found in the P0 live e2e): an empty
  branch diff reads as "the work never happened" — when the diff is empty, feed the
  tasks' final outputs into the validation brief.
- [x] Chat-turn boot recovery (2026-07-05): a restart mid-concierge-turn re-dispatches
  any chat whose last message is still inbound — the macro-review recovery's sibling.

- Working-tree-less delivery merge (see P2 last box).
- Background error surfacing (L6): scheduler/gitops errors reach a visible surface
  (Activity feed or the toast queue), not just the log.
- Error taxonomy (L6): user-actionable vs telemetry, so toasts don't train the operator
  to dismiss.
- Checks-command scoping nit dissolves into P1 per-repo detection.
