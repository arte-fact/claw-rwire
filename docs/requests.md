# Requests: the unit above tasks (git-backed)

> Status: **draft for discussion** — decisions marked ⚖ at the end. Born from the concierge's
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
| Workspace | Repository (`git init` at scaffold; `.gitignore` seeds `target/`, `node_modules/`, …) |
| Request | Branch `req/<short-id>-<slug>`, cut from `main` at plan approval |
| Producer/stage pass (successful) | Commit (harness-made, structured message + trailers: `Request:`, `Task:`, `Run:`) |
| Task | A contiguous commit range on the branch |
| Task restart (fresh attempt) | `reset --hard` to the task's start commit — **fixes the dirty-tree inheritance flaw** |
| Request validation | Review of `main...branch` — the *whole diff against the original ask*, not per-task prose |
| Delivery | Merge to `main` (`--no-ff`, request trailer) — the request's ID is queryable in history forever |
| Rollback before delivery | `reset` branch to before task N / drop the branch — main never saw it |
| Rollback after delivery | `git revert -m1 <merge>` — a new commit, honest history |
| "Restore just that file" | `git checkout <run-commit>^ -- path` |

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
```

Git execution: shell out to `git` via the existing blocking-helper discipline (no libgit2
dep); every invocation workspace-jailed; commits authored as `claw <agent-id@claw>` so
history shows *which agent* made each pass.

## Phases

- **G0 — git-ify workspaces** (no UI): scaffold `git init` + `.gitignore`; lazy-init
  existing workspaces on first commit; harness commit-per-successful-pass; restart resets
  the tree to the task's start commit. *Standalone value: forensic diffs exist, the restart
  flaw is fixed, and `git log` already tells the story.*
- **G1 — the Request entity**: table + `request_id`; ask → request → plan under it; branch
  at approval; lane scoped to the active request; auto-merge when the last task lands
  (validation step stubbed as auto-pass).
- **G2 — diff surfaces**: per-pass diff in the Peek, request page with global diff,
  validator prompts carry the pass diff.
- **G3 — controls + macro validation**: the request-level review step (agent or human gate),
  Deliver/Rollback/Abandon affordances, file-level restore, post-delivery revert.
- **G4 — parallel requests** via worktrees (design spike first).

## ⚖ Decisions to settle

1. **Commit granularity**: per successful producer pass (recommended — finest rollback and
   diff-per-review; a task is a range) vs per task (cleaner log, coarser rollback).
2. **Merge policy**: `--no-ff` merge commit (recommended — task commits stay browsable on
   `main`) vs squash per request (linear `main`, one commit per ask; history of passes
   lives only in the request record).
3. **v1 concurrency**: accept one-active-request-per-workspace with a FIFO queue?
4. **Chat-born edits**: quick asks the concierge answers *without* delegating never touch
   files today — keep it that way (Requests only exist through `ask`)? (Recommended: yes.)
5. **Naming**: `Request` (matches the `ask` tool's own vocabulary and the concierge's
   language) vs `Story` / `Change`.
