# Requests: the unit above tasks (git-backed)

> Status: **decided 2026-07-03** (decision record at the end) — ready for G0. Born from the concierge's
> own harness review (no rollback, no diff review) + the observation that plan → tasks →
> validation already *behaves* like one unit without being one.

## The problem, three symptoms of one gap

1. **No rollback** — "revert that file to before the last build step" has no answer; worse,
   a task *restart* resets its context to a fresh attempt but **inherits the dirty tree**
   (a latent correctness flaw today).
2. **Review reads prose, not diffs** — Critic and the human gate judge the producer's
   *claims*; nobody sees what actually changed on disk, per pass or overall.
3. **No macro handle** — a user ask explodes into 5 tasks that interleave with everything
   else on the board; nothing tracks "that thing I asked for" from plan to delivery, and
   nothing can accept or reject *it* as a whole.

All three are the same missing primitive: **the user's request as a first-class,
version-controlled unit of change.**

## The Request

A **Request** materializes one user ask, from the moment the concierge delegates it until
its work is merged (or discarded). It owns the plan, the tasks, the runs, and — because it
maps onto a git branch — the diff, the review, and the rollback story for free.

```
requested → planning → plan-review → executing → validating → delivered
                ↘ rejected                ↘ abandoned / rolled-back
```

| Domain concept | Git concept |
|---|---|
| Workspace | **Meta-repo**: tracks `AGENT.md`, `STANDARDS.md`, `docs/`, `skills/`; ignores `projects/` wholesale (see Jurisdictions) |
| Project (`projects/<name>`) | Its **own sovereign repo** — init'd at birth or cloned with its upstream intact |
| Request | Branch `req/<short-id>-<slug>` in **each repo it touches**, cut from that repo's `develop` at first touch (git flow) |
| Producer/stage pass (successful) | Commit (harness-made, structured message + trailers: `Request:`, `Task:`, `Run:`) |
| Task | A contiguous commit range on the branch |
| Task restart (fresh attempt) | `reset --hard` to the task's start commit — **fixes the dirty-tree inheritance flaw** |
| Request validation | Review of `main...branch` — the *whole diff against the original ask*, not per-task prose |
| Delivery | `--no-ff` merge to `develop` per repo; remote-bearing repos also **push** the request branch + `develop` (PR creation: later phase) |
| Rollback before delivery | `reset` branch to before task N / drop the branch — main never saw it |
| Rollback after delivery | `git revert -m1 <merge>` — a new commit, honest history |
| "Restore just that file" | `git checkout <run-commit>^ -- path` |

## Repo jurisdictions (why not one workspace repo)

A workspace mono-repo collides with project reality: a project that gains its own `.git`
(or arrives via `git clone`) shows up in an enclosing repo as a **gitlink** — a pinned SHA,
not files — so workspace-level commits would silently stop capturing project changes. And
the decisive constraint: *cloned repos must stay sovereign* (intact SHAs, their own
branches, the ability to push upstream), which rules out subtree/submodule absorption.

So: two layers that never see each other.

- **Project repos** (`projects/<name>/.git`) own all product code. Request branches,
  commits, diffs, rollback, and delivery happen here, on the **git flow model**: the
  harness ensures a `develop` integration branch (created at repo init, or on first
  request for clones that lack one); request branches cut from `develop` and merge back
  `--no-ff`; `main` is release-only, and promoting `develop`→`main` stays a separate,
  explicit act (later phase). Remote-bearing repos push the request branch and `develop`
  at delivery — **request = branch** upstream too; PR creation comes later.
- **The workspace meta-repo** owns the workspace's own material (`AGENT.md`,
  `STANDARDS.md`, `docs/`, `skills/`) and gitignores `projects/` entirely. Cheap, and it
  gives the standards memo and agent-written docs the same diff/rollback story.

**A Request is therefore a changeset over 1..n repos:**

```sql
CREATE TABLE request_repos (
  request_id   TEXT NOT NULL,
  repo         TEXT NOT NULL,   -- '' = workspace meta-repo, else 'projects/<name>'
  branch       TEXT NOT NULL,
  base_commit  TEXT NOT NULL,   -- develop@first-touch (for a fresh clone: upstream HEAD)
  merge_commit TEXT,
  PRIMARY KEY (request_id, repo)
);
```

The harness commit step generalizes: after a successful pass, find every dirty repo
(meta-repo + each `projects/*/.git`), lazily cut the request branch on first touch, commit
in each with the same trailers. One repo remains the common case; the diff view groups by
repo; rollback resets per repo; delivery merges (or pushes) each. Agents get a
standards-level "don't commit — the harness commits" rule; an agent commit on the request
branch degrades nothing (still inside the rollback boundary).

## What existing pieces become

- **`ask`** creates the Request row (status `planning`) *and then* the plan task under it —
  the plan gate is absorbed: **approving the plan = opening the branch**; request-changes
  keeps replanning inside the same Request; rejecting closes it (nothing ever touched a
  branch).
- **Tasks** gain `request_id`; the scheduler's lane picks only from the **active request**
  (see Concurrency). `plan_id` stays for proposal mechanics.
- **The harness commits, never the agents.** After each *successful* producer/stage run:
  `git add -A && git commit` with the structured message. Failed passes leave the tree
  dirty on purpose — the revise loop's context matches the disk. Validators are read-only
  and never commit. No model ever needs git discipline.
- **A new request-level validation step** runs when the last task lands: Critic (or the
  human gate, per workspace auto-approve) reviews the branch diff against the *original
  ask verbatim* — the macro review that doesn't exist today. Pass → merge → notify the
  origin chat with the receipt: *"Delivered 'difficulty setting' — 5 tasks, +412/−67
  across 9 files"* (and `board_status` reports requests, not just tasks).

## Review surfaces (what the UI gains)

- **Peek / run entry**: a Diff disclosure per pass — `git show` of that run's commit
  (rendered with the existing `DiffLine` vocabulary; git computes, we display).
- **Request page** (`/w/:wid/requests/:rid`): the story in one scroll — the ask verbatim →
  the approved plan → the task timeline with per-task diff stats → the **global diff** →
  the validation verdict → Deliver / Roll back / Abandon.
- **Board**: cards carry a request badge; a Requests rail shows each request's progress
  (3/5 tasks · +212/−40) so the user tracks *asks*, not task confetti.
- **Reviewers get diffs in-context**: the validator prompt includes the pass's diff (stat +
  capped patch), so "all tests pass" claims are checked against what actually changed.

## Concurrency ⚖

One working tree can serve one branch. Two models:

- **v1 (proposed): one active request per workspace.** Requests queue FIFO behind the
  active one; the lane already runs one task at a time, so this costs little and ships
  fast. Independent asks can still parallelize *across* workspaces.
- **v2: `git worktree` per request** — true parallel requests in one workspace, each branch
  in its own tree, the scheduler grows per-request lanes. Real work (worker paths, tool
  jails, port/file collisions between concurrent builds) — deliberately later.

## Schema sketch

```sql
CREATE TABLE requests (
  id             TEXT PRIMARY KEY,
  workspace_id   TEXT NOT NULL REFERENCES workspaces(id),
  origin_chat_id TEXT,
  title          TEXT NOT NULL,          -- short, from the ask's first line
  ask            TEXT NOT NULL,          -- the user's words, verbatim (the validation contract)
  status         TEXT NOT NULL,          -- the lifecycle above
  branch         TEXT,                   -- req/<short-id>-<slug>, set at plan approval
  base_commit    TEXT,                   -- main@branch-point
  merge_commit   TEXT,                   -- set at delivery
  created_at     TEXT NOT NULL,
  updated_at     TEXT NOT NULL
);
ALTER TABLE tasks ADD COLUMN request_id TEXT;   -- plan task included
-- plus request_repos (see Jurisdictions) — the 1..n (repo, branch) set per request
```

Git execution: shell out to `git` via the existing blocking-helper discipline (no libgit2
dep); every invocation workspace-jailed; commits authored as `claw <agent-id@claw>` so
history shows *which agent* made each pass.

## Roadmap (tick as shipped; every phase ends gate-green + live-verified)

### G0 — git-ify the jurisdictions (no UI; standalone value) — ✅ shipped 2026-07-03
- [x] `src/gitops.rs`: jailed `git` shell helpers (init, status --porcelain, add+commit with
      trailers, rev-parse, reset --hard, branch/checkout) — blocking-helper discipline,
      commits authored `claw <agent-id@claw>`
- [x] Workspace scaffold creates the **meta-repo**: `git init` + `.gitignore` (`projects/`
      excluded wholesale) + initial commit; lazy-init on first touch for existing workspaces
- [x] Project birth creates a **sovereign repo**: `git init` + seeded `.gitignore`
      (`target/`, `node_modules/`, …) + initial commit + `develop` branch
- [x] After each successful producer/stage pass: commit every dirty repo (meta + projects)
      with `Task:`/`Run:` trailers (no requests yet — commits land on `develop`/meta HEAD)
- [x] Task **restart** resets each repo the task touched to its pre-task commit — the fresh
      attempt finally gets fresh files (regression test for the dirty-tree inheritance flaw)
- [x] Failed passes deliberately leave the tree dirty (revise-loop context matches disk) —
      asserted by test
- [x] Live smoke: run a real task; `git log` shows one commit per pass with trailers

### G0.5 — hardening + the agents' read-only git surface — ✅ shipped 2026-07-03
Born from "does git deserve dedicated agent tools?" — read surface yes, write monopoly
stays with the harness, and the real safety net is fencing bash-git.
- [x] Harness git is hermetic: every `gitops` invocation runs `core.hooksPath=/dev/null`
      (an agent-writable `.git/hooks` could otherwise execute code in the server process
      on the commit sweep) and cuts off global/system git config — asserted by test
- [x] `repo_status` / `repo_log` / `repo_diff` tools (read-only, capped): producers and
      validators get them task-anchored — `since:"task-start"` diffs against the
      `task_repos` base, state bash-git can't reach; the concierge gets
      workspace-parameterized variants next to `board_status`/`read_file`
- [x] Bash git fence: read-only subcommand safelist (status/log/diff/show/…/fetch/clone),
      writes denied with a pointer to the harness contract; config-injection and
      exec-transport vectors (`-c`, `--git-dir`, `--upload-pack`, `ext::`) blocked
- [x] Agent bash is credential-hermetic: no git prompts, no global config (credential
      helpers), no ssh-agent — public clone/fetch works, pushes find no ambient identity
- [ ] `clone` as a dedicated jailed tool — deferred until a real workflow needs it

### G1 — the Request entity — ✅ shipped 2026-07-03
- [x] `requests` + `request_repos` tables; `tasks.request_id` migration
- [x] `ask` → Request row (`planning`) → plan task under it; plan approve/reject drives the
      request lifecycle (approve = executing; reject = rejected, no branch ever cut)
- [x] Plan request-changes replans **within the same Request**
- [x] First touch of a repo by a request: ensure `develop`, cut `req/<short-id>-<slug>` from
      it, record `request_repos` row (clone edge: base = clone HEAD)
- [x] Scheduler lane scoped to the **active request**; FIFO activation of queued requests
- [x] Delivery v0: last task done → `--no-ff` merge to `develop` per repo (validation
      stubbed as pass) → origin-chat receipt ("Delivered … N tasks, +A/−D across F files")
- [x] `board_status` + the chat digest report requests, not just tasks
- [x] Live e2e: one ask through plan → tasks → merged `develop`

### G2 — diff surfaces — ✅ shipped 2026-07-03
- [x] `git show`/`diff` plumbing parsed into the existing `DiffLine` rendering
- [x] Peek run entry: per-pass **Diff** disclosure (stat + patch, capped)
- [x] Request page `/w/:wid/requests/:rid`: ask verbatim → plan → task timeline with
      per-task diff stats → global diff → status
- [x] Board: request badge on cards + a Requests rail (progress · diffstat)
- [x] Validator prompts include the pass diff — claims checked against changes
- [x] Live verify on a real produce⇄review loop

### G3 — controls, macro validation, push
- [ ] Request-level validation when the last task lands: review of `develop...branch`
      against the **ask verbatim** (agent reviewer; human gate honors workspace
      auto-approve)
- [ ] Deliver / Abandon / Roll-back affordances on the request page (pre-merge: reset to
      before task N or drop the branch)
- [ ] Post-delivery rollback: `git revert -m1 <merge>` from the request record
- [ ] File-level restore (`checkout <run>^ -- path`) from a run's diff view
- [ ] Delivery **push** on remote-bearing repos: request branch + `develop` (explicit,
      gated; PR creation deferred)
- [ ] Live e2e including a rollback and a push to a scratch remote

### G4 — parallel requests (worktrees) — later
- [ ] Design spike: worker paths, tool jails, build/port collisions between concurrent trees
- [ ] `git worktree` per active request; per-request lanes in the scheduler


## Decision record (settled 2026-07-03)

1. **Commit granularity: per successful producer pass.** A task is a commit range; rollback
   and review keep pass-level resolution.
2. **Merge policy: `--no-ff`.** The merge commit carries the request trailer; pass commits
   stay browsable.
3. **Concurrency v1: one active request per workspace, FIFO queue.** Worktrees are the G4
   unlock.
4. **Remotes: push the branch in v1, on the git flow model.** No PR creation yet.
5. **Git flow: classic `develop` integration branch.** Harness ensures `develop`; requests
   cut from and merge to it; `main` is release-only (promotion is a later, explicit act).
6. **Scope: ask-only.** Every file change flows through a Request born from delegation;
   concierge quick answers never touch files.
7. **Naming: Request.**
8. **Clone edge: base at clone HEAD.** A repo cloned mid-request gets its request branch cut
   immediately; the diff is exactly what the request did to pristine upstream.
