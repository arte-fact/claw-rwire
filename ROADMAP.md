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
> **Companion: rwire `RELEASE_ROADMAP.md`** (Phases 5–6, executed by the rwire agent) — the framework
> improvements P4a and P5c adopt. Improve the framework first; claw consumes the result.

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

## Upstream-first — the framework leads (rwire `RELEASE_ROADMAP.md`)

Improve rwire before committing claw's UI work; the **rwire agent** executes its RELEASE_ROADMAP. Two
of Platform II's surfaces are **adoptions**, not hand-rolls — the promotion-upstream constraint
realised (Breadcrumb/Progress precedent):

- **P4a (conversation)** adopts rwire's `Chat`/`ChatTranscript` family (rwire **F7**) on the extracted
  runtime (rwire **Phase 5**), with **seamless infinite-scroll history** (rwire **F1** scroll-sentinel)
  — this **supersedes the drafted "↑ load older" button**. claw's app-specific transcript content (tool
  cards, memory lines, review gates, streaming turns) plugs in via F7's **`ChatItem` trait** (+ a
  chrome-free `row()` escape hatch); collapsible rows become native **`<details>`/`<summary>`**
  (`El::Details`/`El::Summary`, the first F7 brick — shipped upstream `6260436`), retiring claw's
  server-round-trip toggles (`tool_toggled`/`disclosure_row`). Benefits from rwire **T1** keyed diffing.
- **P5c (per-branch file tree)** adopts rwire's `FileTree`/`FileExplorer` (rwire **F2**) in read-only
  mode; `ui/files.rs` migrates onto the managed `FileExplorer`.
- **P4c logs** can ride rwire's `StreamedContent` (rwire **F1**) when a log is large, rather than one
  giant frame — optional, not blocking.

The **pipeline + CD work is claw-native** and does **not** block on rwire: P4b (`pipeline.yaml`), P4c
(logs), P5a (`develop→main`), P5b (origin + keys) proceed in parallel with the framework track. Each
adoption lands with a migration that **deletes local scaffolding** (`ui/chat.rs`, `ui/thread.rs`,
`ui/files.rs`) — the migration pressure-tests the upstream component's genericity.

**Critical path:** rwire's own status note flags **Phase 5 → F1 → F7** as the hot path that Platform II
blocks on. But the first F7 brick — native `<details>`/`<summary>` (`El::Details`/`El::Summary`, shipped
`6260436`) — is adoptable **now**: claw can retire its server-round-trip disclosure toggles ahead of the
rest of the component, an early non-blocking win.

## P4 — Legibility: a visible conversation and a visible pipeline

The "make it legible" axis: one thread you can scroll back through, one recipe you can read, and the
CI/CD output you were previously denied.

### P4a — One conversation surface, adopting rwire's `Chat`

Upstream-first: the concierge, agent threads, and the task Thread tab all render through rwire's
first-class `Chat` component (rwire F7) — claw stops hand-rolling and deletes its scaffolding.
**Depends on** rwire Phase 5 (runtime extraction) + F1 + F7 — the **critical path** for Platform II
(rwire flags Phase 5 → F1 → F7 as the hot path claw blocks on).

- [ ] **Adopt the upstream `Chat`/`ChatTranscript`**: all three surfaces render through it (author
      rail, markdown body, `Composer`, writing state); delete the local `ui/chat.rs`/`ui/thread.rs`
      scaffolding. *Site*: `ui/chat.rs`, `ui/agent.rs`, `ui/thread.rs`.
- [ ] **App content via the `ChatItem` trait**: claw's rich transcript items — tool cards, memory
      lines, review/approval gates, streaming turns — implement F7's `ChatItem` (`key`/`author`/`body`
      + defaulted `time`/`tag`/`detail`/`groups_with`, and a chrome-free `row()` escape hatch for
      system lines and gate rows). Design: rwire `docs/chat-component-design.md`. *Site*:
      `ui/run_chain.rs`, `ui/chat.rs`.
- [x] **Native disclosure (adoptable now)** — shipped `e5a66c9`: collapsible tool/diff/memory/check
      rows render as native `<details>`/`<summary>` (`El::Details`/`El::Summary`, the shipped
      `ChatDetail` pattern); the browser owns the toggle (zero-latency, free a11y). Retired the
      server-round-trip toggles — `App::tool_toggled` + the `toggle_tool` handler are gone. *Site*:
      `ui/run_chain.rs`, `ui/{chat,task_detail,request_page}.rs`.
- [x] **Seamless history — no button** — shipped `2a2ae5a` (agent thread) + `ec7f484` (concierge):
      both conversation surfaces window to the newest N turns; older turns arrive by **infinite
      scroll** (rwire's `on_visible` sentinel, one page in flight via its re-keyed binding), not a "↑
      load older" button. The agent thread reuses its existing `OPEN_AGENT` window; the concierge
      gains a mirror `OPEN_CHAT` (chat_id, window) store that grows a page on `ExpandChat` and resets
      lazily on a differing chat_id (no switch hook). Pages are id-ordered (`load_thread_windowed`
      LIMIT N, oldest-first) — simpler than and equivalent to a `created_at,id` cursor; the
      memory-fold line stays above the oldest loaded page; the subtree stays keyed by chat id. *Site*:
      `bridge/{chat_turn,agent_thread}.rs`, `db/chat.rs`, `ui/{chat,agent}.rs`.

### P4b — An agent-owned `pipeline.yaml`, with a floor claw enforces

Turn the inferred, invisible recipe into a file the agents maintain — without letting them edit away
the gate.

- [x] **Read a root `pipeline.yaml`** — shipped `1098d2d` + `6d89ab5`: `src/pipeline.rs` reads a
      project-root `pipeline.yaml` (checks + preview) via `serde_yml`; `ci::checks_for` and
      `preview::command_for` consult it first, then the per-repo override, then the kind recipe (safe
      fallback when absent/malformed). `kinds::detect` **seeds** the first draft — the rwire
      template's `projects/hello` ships one. The `settings` override families stay as legacy fallback
      rather than being deleted; the file simply wins. *Site*: `src/pipeline.rs`, `ci.rs`,
      `bridge/preview.rs`, `bridge/template.rs`.
- [x] **The agents own it** — shipped `a664c5f`: the producer's project layer appends a
      `PIPELINE_NOTE` (read + maintain `pipeline.yaml` in the same pass); the rwire `STANDARDS.md`
      carries the matching review rule. *Site*: `scheduler/mod.rs`, `bridge/template.rs`.
- [x] **claw enforces the floor** — shipped `bded391`: `ProjectKind::floor()` (cargo kinds
      `fmt`·`test`·`build`; others empty) is merged into `ci::checks_for` on top of the resolved
      base, so the floor always runs and gates — feeding *both* the per-pass gate and the macro gate
      (`control.rs`) from one resolution point. An agent can't trim a check to weaken it; `build` is
      the kind-derived floor stage, not an editable field. Degrades to an empty floor for non-cargo
      kinds. *Site*: `kinds.rs`, `ci.rs`.

### P4c — Persisted CI/CD logs

Stop discarding the output; make every stage's log openable.

- [ ] **Full per-stage logs**: CI stops cutting to a 2 000-char tail and writes the whole log
      (mirror the preview logfile pattern, `preview.rs:538`); a logs drawer surfaces it on the task
      thread and (P5c) the project page — a large log can stream through rwire's `StreamedContent`
      (rwire F1) rather than one giant frame. *Site*: `ci.rs`, a log store, `run_chain::disclosure_row`.

### P4d — The planner, made present

The planner is the owner's most consequential turn — it decides the whole task breakdown — yet it is
the one actor with **no presence in the interface**. `orchestrate::launch_planning` runs the owner
through the **non-streaming** `run_turn` (`orchestrate.rs:145`), posts only the finished proposal, and
**persists nothing**: the turn's reasoning evaporates, the owner's agent thread never shows it, and the
operator watches an opaque "planning…" badge for the whole wait — the step *feels* slow precisely
because it is invisible. Producer runs already solved this: they stream via `run_turn_steered` + an
`EventSink` into `App.live_turns` (`bridge/live.rs`), rendered as a live entry
(`live_entry`/`live_stream_body`). Give the planner the same treatment — legibility over the one turn
that shapes everything downstream. (Rides on P4a's `ChatEntry`; no rwire dependency of its own.)

- [ ] **Stream the planning turn**: switch `launch_planning` from `run_turn` to `run_turn_steered`
      with a live sink, reusing the producer stream path — tool calls and the reply arrive as they
      happen, not all at once at the end. *Site*: `bridge/orchestrate.rs`, `bridge/live.rs`.
- [ ] **A planning presence where the request lives**: the concierge (`origin_chat`) and the plan/
      request page show a live "*owner* is planning…" entry streaming its work — the same live entry
      the board shows for a producer, rendered through P4a's `ChatEntry`. Retires the opaque status
      wait. *Site*: `ui/chat.rs`, `ui/request_page.rs`.
- [ ] **Persist the planning turn as a run**: record it in `task_runs` (`RunKind::Planning`) against
      the plan task, with its transcript — so the proposal has a forensic "why", scrollable like a
      producer pass, instead of an ephemeral one-shot. *Site*: `bridge/orchestrate.rs`, `db/task_runs.rs`.
- [ ] **The planner in the owner's thread**: the owner agent's thread (`ui/agent.rs`) includes its
      planning turns as authored entries (a `Plan` phase tag), so its working life reads producing ·
      reviewing · **planning**, not just the first two. *Site*: `bridge/agent_thread.rs`, `ui/agent.rs`.
- [ ] **The proposal points back**: `proposed_view` links each proposed breakdown to the planning run
      that produced it — "why these tasks" is one click from the plan. *Site*: `ui/task_detail.rs`,
      `ui/request_page.rs`.
- [ ] **Speed the wait, not just its perception** (secondary): the planning turn is a light three-tool
      loop (`propose_tasks` / `list_tasks` / `cancel_task`) — the latency is the LLM round(s) + a cold
      context cache, not the toolbox. Consider a faster model for the planning role and cache-warming;
      the streaming above makes whatever wait remains legible. *Site*: `db/resolution.rs` (per-role
      model), `bridge/orchestrate.rs`.

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
      drawer, the live preview link (`port_for`), and a **read-only file tree of the selected branch**
      rendered with rwire's upstream `FileTree` (rwire F2, read-only mode) over
      `git show <branch>:<path>` / the worktree-less tree. CI grows a **branch dimension** (today
      `CheckOutcome` is per-run, aggregated per request). *Depends on* rwire F2. *Site*:
      `ui/project_page.rs`, `state/views.rs`, `bridge/requests/view.rs`, `ci.rs`.
- [ ] **Migrate `ui/files.rs` onto the upstream `FileExplorer`** (rwire F2, managed mode) — delete the
      local two-pane scaffolding; the Files tab and the branch tree share one component. *Site*:
      `ui/files.rs`.

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
