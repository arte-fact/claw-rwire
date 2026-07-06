# Platform II: legibility, release topology, and the config the agents own

> **The active roadmap** (2026-07-06). Supersedes **Platform I** (P0–P3: isola sandboxing,
> first-class projects + derived kinds, per-repo CI, the same-origin preview proxy, the rwire
> template — all shipped 2026-07-05). Platform I is a complete historical record: condensed in
> *Shipped foundation* below, its full box-by-box detail preserved in git history. The four new
> moves are one system: claw already **runs** CI and CD; Platform II makes them **legible** and
> **consistent** — one conversation surface loaded a page at a time, a pipeline the agents own in
> a file you can read, CI/CD output you can actually see, and a two-line branch topology where
> `develop` and `main` are both places you can open. Cadence unchanged: each phase gate-green,
> live-verified, ticked with a commit. Design source: the UX & CD proposal (2026-07-06, artifact).

## Shipped foundation (Platform I, P0–P3)

> **P0 isola on** — agent `bash` + machine checks run in per-lane isola sandboxes
> (`CLAW_SANDBOX=isola`, runtime-detected), lane `claw-<ws>-<req8>`, ~22 s snapshot restore,
> teardown reaps the rootfs. Harness git / DB / server / file tools stay host-side by design.
> **P1 projects first-class** — a project IS its directory; **kind is a pure function over its
> files** (`kinds::detect`) binding the CI recipe, preview command, and prompt layer; sparse
> `settings` overrides only when set. **P1b CI** — per-repo checks at producer-finish, pass-level
> fail-fast (re-run the producer with the red tail, cap 2), results in `task_runs.checks`, a macro
> gate that holds auto-delivery on red. **P2 CD** — every request branch a same-origin
> `/preview/<key>/` URL behind claw's auth gate (rwire `ServerConfig::proxy` + `base_path`,
> upstream); `develop`-staging preview per web project, restarted on delivery. **P3 rwire template**
> — the dashboard scaffolds a previewable `projects/hello` declaring rwire as a public GitHub git
> dep; kind/CI/preview follow from the manifest. *Open from Platform I, folded in below*:
> working-tree-less delivery merge; Critic-on-a-stronger-model; background error surfacing (L6).

## Findings (verified 2026-07-06)

- **Two conversation surfaces, both eager.** Chat (`ui/chat.rs`) renders the whole `app.messages`
  list (older turns only hidden behind a memory-fold line); the agent page (`ui/agent.rs`) has its
  own layout and a `load_older(truncated)` stub. Neither pages from a cursor. The turn widget
  `thread::entry` / `thread::scroll` is **already shared** — the callers diverge, not the atom.
- **The build recipe is inferred and invisible.** `kinds::detect` drives checks + preview + prompt
  + badge; the only tuning is three hidden `settings` key families
  (`project.<ws>.<p>.kind|preview`, `workspace.<ws>.checks`). **No config file exists** (no
  `serde_yaml` anywhere). Clean resolution seams: `ci::checks_for` (`src/ci.rs:152`) and
  `preview::command_for` (`src/bridge/preview.rs:305`), both already "override-then-kind".
- **CI throws its output away.** Only a 2 000-char red **tail** survives per failed check
  (`ci.rs`); green checks store nothing; the full command output is discarded. Previews, by
  contrast, *do* write a logfile (`preview.rs:538`) — the pattern to copy.
- **`main` has never moved.** It holds one "initialize" commit and is never advanced
  (`gitops/requests.rs:1-3`); everything integrates on the hardcoded `DEVELOP` const; delivery is a
  real `checkout develop && merge --no-ff` in the single canonical tree. The design doc
  (`docs/requests.md`) already *intends* a release line — unbuilt.
- **No remote config.** Push is hardcoded `origin develop <branch>` (`gitops/requests.rs:93`);
  https credential helpers are cut off in-sandbox; harness git runs host-side. Nothing stores or
  chooses a remote or a key.
- **The project page is a static read-model.** `ProjectDetail` (`state/views.rs:807`) has no branch
  or checks fields; the page renders a kind badge, an override select, and a request list — its own
  header comment promises "CI-on-develop and preview/staging panels arrive later."

## Elegance constraints (bind every phase)

1. **Derive, don't store — with one deliberate exception.** git IS the registry: a project is its
   directory, its history is `request_repos`. The **`pipeline.yaml` is a file in the repo**, not a
   table — versioned, diffable, agent-owned. Net new *storage* across Platform II is bounded to two
   justified stores: a CI/CD **log** (today discarded) and the admin **SSH-key** store.
2. **One runner, three consumers.** The mandatory floor and the yaml's extra checks run through the
   one `src/ci.rs` runner that the harness gate, the `checks` tool, and the delivery gate share — a
   machine verdict means the same thing everywhere or it means nothing.
3. **Complexity flows upstream.** The working-tree-less merge is generic git plumbing; anything
   reusable by any rwire app lands in the framework (the Breadcrumb/Progress/base-path precedent).
4. **Reuse the seams that exist**: `thread::entry`/`scroll` (P4a), the `checks_for`/`command_for`
   resolution points (P4b), the preview logfile + `disclosure_row` (P4c), the preview supervisor and
   `port_for` (P5), the project-page skeleton (P5c), sparse settings (subsumed by the yaml).
5. **New-file budget**: `src/pipeline.rs`, a log store, an admin key store + its UI surface. Four
   modules, each under the house 300-line cap; a phase that wants a fifth justifies it in its commit.

## Decided model (2026-07-06)

- **Config:** one **`pipeline.yaml` at the project root**, **agent-managed** (authored + kept current
  by the agents, instructed by the system prompt), with a mandatory **format · test · build** floor
  claw enforces and **gates validation** on — the file adds checks, never removes the floor.
- **Branching:** lean **develop + main**. `develop` = the dev instance, `main` = internal staging —
  **both previewable in-app**. Delivery merges into `develop`, CI-gated on the floor. **Release** is a
  user action: merge `develop → main`, then push `main` if an origin is set.
- **Production:** deployed by the operator from **origin/main, outside claw** — claw's job ends at
  "main pushed."
- **Origin:** optional, set **from the project page**; auth via an **admin SSH-key store**, one key
  chosen per project; commits **ahead/behind** origin and **merge-conflict** state shown in-app.

## P4 — Legibility: a visible conversation and a visible pipeline

The "make it legible" axis: one thread you can scroll back through, one recipe you can read, and the
CI/CD output you were previously denied.

### P4a — One conversation surface, lazily loaded

The concierge chat and an agent thread should be the same widget, and neither should ship a thousand
turns to show ten.

- [ ] **Unify the surface**: chat (`/`) and the agent thread (`/w/:wid/agent/:aid`) render through one
      `Thread` component — same author rail, markdown body, composer, spacing. *Site*: `ui/chat.rs`,
      `ui/agent.rs`, `ui/thread.rs` (fold the two callers onto the shared `thread::entry`/`scroll`).
- [ ] **Windowed history**: render the last N turns; `↑ load older` fetches the previous page by
      cursor (`created_at,id`); the memory-fold line sits above the oldest *loaded* page. Replaces the
      full-list read in the bridge thread projection; keep the subtree keyed so a chat switch swaps it.

### P4b — An agent-owned `pipeline.yaml`, with a floor claw enforces

Turn the inferred, invisible recipe into a file the agents maintain — without letting them edit away
the gate.

- [ ] **Read a root `pipeline.yaml`** (build · checks · preview) as the source of truth, resolved at
      `ci::checks_for` and `preview::command_for`; detection **seeds** the first draft, the file wins
      when present. Subsumes the three hidden `settings` override families. *Site*: new
      `src/pipeline.rs`, `ci.rs`, `bridge/preview.rs`, `kinds.rs` (detect → scaffold).
- [ ] **The agents own it**: the P1 prompt layer instructs the producer to author/maintain
      `pipeline.yaml` (format, when to touch it). *Site*: `scheduler/mod.rs` prompt composition,
      the rwire `STANDARDS.md`.
- [ ] **claw enforces the floor**: `format · test · build` always run and **gate validation**
      regardless of the file — an agent can't weaken the gate by editing it. Hardens the existing
      macro gate (`bridge/requests/control.rs:256`) into a floor check. Floor degrades to "must pass
      if the kind defines it" for non-buildable projects.

### P4c — Persisted CI/CD logs

Stop discarding the output; make every stage's log openable.

- [ ] **Full per-stage logs**: CI stops cutting to a 2 000-char tail and writes the whole log
      (mirror the preview logfile pattern, `preview.rs:538`); a logs drawer surfaces it on the task
      thread and (P5c) the project page. *Site*: `ci.rs`, a log store, `run_chain::disclosure_row`.

## P5 — Release topology: develop → main, origin, and the control center

The "make it a place" axis: two branches you can preview, an optional origin you manage in-app, and a
project page that is the CI/CD cockpit.

### P5a — develop → main + Release

- [ ] **Two-line lineage**: generalize the hardcoded `DEVELOP` const into a `develop` (integration) +
      `main` (release) lineage. Delivery still merges feature → `develop`, now **CI-gated on the
      floor**. *Site*: `gitops/requests.rs`, `bridge/requests`.
- [ ] **Release action**: an explicit user act — merge `develop → main`, then push `main` **iff** an
      origin exists. Not automatic. *Site*: `gitops/requests.rs` (a `release()` beside `deliver`),
      `bridge/requests`, the project page.
- [ ] **`main` is previewable**: the preview supervisor gains a `main`/internal-staging instance
      beside `develop`-staging (both testable in-app). *Site*: `bridge/preview.rs`
      (`refresh_staging`/`start_staging`).
- [ ] **Keystone (from Platform I backlog)**: working-tree-less merge (`merge-tree` + `commit-tree` +
      `update-ref`) — decouples the integration line from the single checked-out tree, which both a
      clean Release and P5c's branch-switching need. *Site*: `gitops/requests.rs`, `gitops/worktrees.rs`.

### P5b — Origin + admin SSH keys

- [ ] **Admin SSH-key store**: an admin surface holds keys; private keys stay server-side, scoped to
      admin, out of git and the normal DB export. *Site*: a key store + `ui/` admin.
- [ ] **Per-project remote + key**: set the remote and choose a key **from the project page**
      (optional — no origin ⇒ Release just advances `main` locally). Push uses the chosen key
      host-side; replaces the hardcoded `origin` refspec. *Site*: `ui/project_page.rs`,
      `gitops` push (`push_refs`).
- [ ] **Sync status in-app**: commits **ahead/behind** origin (`git rev-list --count`) + a
      **merge-conflict** signal (dry-run merge). *Site*: `gitops`, `bridge/requests/view.rs`,
      `state/views.rs`.

### P5c — The project page as CI/CD control center

- [ ] **Branch switcher → per-branch pipeline · logs · preview · files.** `ProjectDetail` gains a
      branch list, a per-branch pipeline (floor + extra checks, statuses + durations), the logs
      drawer, the live preview link (`port_for`), and a read-only file tree of the selected branch
      (`git show <branch>:<path>`, or the worktree-less tree). CI grows a **branch dimension** (today
      `CheckOutcome` is per-run, aggregated per request). *Site*: `ui/project_page.rs`,
      `state/views.rs`, `bridge/requests/view.rs`, `ci.rs`.

## Backlog (harvested, scheduled by pull)

- **Ops (Platform I, L1):** put Critic on a stronger model than the producers — two fields on the
  System page; do it when picking the next model lineup.
- **Background error surfacing (L6):** scheduler/gitops errors reach a visible surface (Activity
  feed or the toast queue), not just the log. Naturally pairs with P4c's log store.
- **Error taxonomy (L6):** user-actionable vs telemetry, so toasts don't train the operator to dismiss.
- **Output-only requests confuse the macro review:** an empty branch diff reads as "the work never
  happened" — when the diff is empty, feed the tasks' final outputs into the validation brief.
- **P3 success metric (still open):** a fresh "build me an X" ask in an rwire-template workspace
  ships a previewable app with zero operator touches between plan approval and Deliver — the
  deterministic pieces are green; needs one agent-driven request to Deliver.
