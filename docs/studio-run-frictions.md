# Studio run frictions — live end-to-end (Rust roguelike)

A grounded write-up from driving the **Studio** workspace end-to-end through the web UI to build a
playable Rogue-clone CLI game in Rust (model: `deepseek/deepseek-v4-flash` via OpenRouter). The
build **succeeded** — the autonomous pipeline produced a coherent, compiling, actually-playable
roguelike (procedural dungeon, moving `@`, chasing `M` monsters, bump combat, `!` potions / `*`
gold, `>` stairs, 3-floor win, HP-0 lose, 5 passing tests). These notes are the frictions hit on
the way, severity-ordered, each with concrete evidence.

## Workflow / orchestration

1. **Reviewer (Critic) tool-calls degrade with this model — failed reads + leaked control tokens.**
   On *Terminal rendering*, the Refine iter-2 Critic made 10 `read` calls that **all failed** with
   bare/wrong paths (`render.rs`, `src/render.rs`, `lib.rs`, `main.rs`, `projects`, `.`) instead of
   the real `projects/rogue-clone/src/…`, and its stored `output` was literally
   `<｜DSML｜tool_calls｜>` — raw deepseek tool-call control tokens leaking through the
   OpenAI-compatible bridge. Net effect: the reviewer couldn't read the code it was reviewing and
   produced no usable verdict. Forge's reads in the same project succeed (`✓`), so the agents don't
   share a consistent cwd / path convention, and the tool-call parser doesn't strip the model's
   native DSML framing.
   **Fixed:** the validator's context now lists the workspace's real file paths
   (`workspace::list_files` → `scheduler::validator_context`), and `read` resolves a guessed path by
   unique suffix/basename (`coder::find_by_suffix`) so `render.rs` finds `projects/…/render.rs`.
   Leaked DSML is recovered into runnable calls (`client::recover_leaked_tool_calls`, already in) and
   any residual wrapper is scrubbed from stored output (`scheduler::strip_dsml`).

2. **Verdict not extracted from a perfectly good prose review.** Terminal rendering's iter-0 Critic
   wrote a precise, correct, actionable review ending in **"Verdict: revise — change the inner
   `_ => continue` to `_ => return None`."** Yet `task_runs.verdict` was stored as `None`. The loop
   treated `None` as "not pass" and burned both Refine iterations, then escalated to human — with no
   clean verdict/feedback surfaced at the gate. The verdict parser doesn't recognise the
   `Verdict: <x>` prose form the model actually emits.
   **Fixed:** `parse_verdict` now falls back to `parse_verdict_prose`, which extracts
   `(decision, feedback)` from a `Verdict: <pass|revise|reject> — …` line when the tool wasn't
   called (`scheduler` verdict path).

3. **Silent iteration burn → opaque human escalation.** Because of (1)+(2), the task hit
   `max_iterations` and escalated via `on_exhaust: human`, but the board just shows `awaiting` at
   Refine. The *reason* (failed reviews) is only discoverable by expanding the run chain and seeing
   `✗✗✗` + DSML garbage. The escalation should say *why* it escalated.
   **Fixed:** an on-exhaust escalation now carries a reason ("escalated — not approved after N
   review attempt(s): <last feedback>") that `park_awaiting` records on the task (`tasks::set_result`)
   and surfaces in the origin-chat ping + activity feed — and the ping now fires on escalation, not
   only on the first park (escalations land at iteration > 0, which used to park silently).

4. **Plan dependency graph is incomplete.** The planner made *Multi-floor / game-loop integration*
   `blocked_by` Core types + Terminal rendering — but **not** *Player movement, monster AI & combat*.
   So integration ran while `monster.rs` was still an unimplemented stub. It only worked out because
   the integration task *re-implemented the chase AI inline in `game.rs`* (lucky compensation, also a
   scope-creep symptom). The intended `monster.rs::update_monsters` never gets wired in — Player
   movement's whole deliverable becomes dead code.

5. **Single-worker serialization makes the whole run slow and order-sensitive.** One task runs at a
   time per workspace. A slow/looping task (Terminal rendering's double Refine; Multi-floor's long
   Critic review) blocks everything behind it — including my just-submitted change-request rework on
   Core types, which sat `pending` for minutes behind an unrelated task.

6. **Downstream tasks start before the upstream human-Approve gate clears.** Terminal rendering
   began executing while Core types was still `awaiting` my approval. When I then *requested changes*
   on Core types, a dependent was already mid-flight on the pre-change code — the human gate and the
   scheduler race.

7. **LLM timeout handling is invisible.** Player movement's Execute stage failed with
   `request failed: timed out reading response` (deepseek timed out mid-task, after `player.rs` was
   written but before `monster.rs`). The task silently reverted to `pending` to retry — no surfaced
   error chip on the card, no "retrying after timeout" signal. From the board it just looks stuck.

8. **Every subtask carries its own Approve gate → approval fatigue.** With the seeded
   Execute→Refine→**Approve(human)** pipeline applied to *each* of 5 decomposed subtasks, a single
   project needs ~5 separate human approvals. Worse, a task that escalates at Refine *and* then runs
   its Approve step parks the human **twice** (Terminal rendering: once at Refine-exhaust, once at
   Approve). Consider auto-passing trivial/leaf subtasks, or gating only the final deliverable.

9. **Approve step burns a Forge "finalize" LLM pass even on trivial tasks**, and that finalize pass
   isn't scope-limited: finalizing *Core types* also rewrote `game.rs` and `render.rs` (files owned
   by later tasks). Scope creep from a step that should just prepare the existing work for sign-off.

10. **Duplicate task churn from planning.** Two identical "Scaffold Rust project" tasks were created;
    one ran, one was left `cancelled` in Closed. Minor, but it's visible noise on the board.

15. **The Refine gate passed code that doesn't compile.** Player movement's retry implemented
    `monster.rs::update_monsters` but with a borrow-checker bug (`let monster_list =
    std::mem::take(&mut state.monsters);` then `for mut monster in monster_list` *moves* it, then
    `monster_list.iter()` is used after the move → `error[E0382]`). The task's Critic/Refine step
    **passed** it and advanced it to the human Approve gate — i.e. the autonomous reviewer signed off
    on a tree where `cargo build` fails, **regressing a previously-working, playable build**. A human
    catching this (I requested changes citing the exact error) is the only thing that stopped a
    broken build from reaching "Done". The Refine validator needs a hard `cargo build`/`cargo test`
    gate it can't pass-over. (Also: `monster.rs::update_monsters` duplicates the chase AI that the
    game-loop task already inlined in `game.rs` — see friction #4.)

16. **A stated deliverable silently fell through.** The spec explicitly listed *"Include a brief
    `ARCHITECTURE.md` explaining the design and how to play"* and tests for FOV/pathfinding. The
    planner allocated **no task** for the docs deliverable, no agent produced `ARCHITECTURE.md`, and
    no Critic flagged its absence — the project finished "Done" without it. Decomposition + review are
    spec-*local* (each subtask checks its own slice); nothing checks the *whole* deliverable list at
    the end. A final "deliverables checklist" gate against the original request would catch this.

## UX / surfacing

11. **The "Plan" review gate doesn't show the plan.** The parked Plan task's Overview is just the
    original request verbatim, and Activity says "No runs recorded yet." The actual plan (the 5 child
    tasks + dependency chain) only exists as sibling cards on the board — so to review *the plan* you
    must mentally reassemble it from the column. The gate should summarise the decomposition it's
    asking you to approve.

12. **`stopped` is the wrong word for blocked/queued children.** Pre-created child tasks waiting on
    the Plan gate show status `stopped` (implies a user stopped them) rather than
    `queued`/`blocked — awaiting plan approval`. The first-in-chain child showed `stopped` with no
    blocker badge at all, so "why isn't this running?" is unanswerable from the card.

13. **No "agent is generating" heartbeat.** A long deepseek generation (or a stalled one) looks
    identical to a hang: the card spins, no files change, the server log shows nothing. There's no
    "thinking… (45s)" / token-streaming indicator at the task level, so a 6-minute Execute stage is
    indistinguishable from a dead one.

## Environment trap (not the app's fault, but worth a guard)

14. **PWA service worker serves a stale shell when the server is unreachable → permanent
    "Reconnecting…".** A browser that can't reach the server still gets the cached app shell from the
    SW (`SHELL=['/', …]`), which then loops on "Reconnecting…" forever with no hint that it's serving
    a cached/offline copy. Clearing the SW cache then exposed the real `ERR_CONNECTION_REFUSED`. A
    visible "offline / serving cached copy" banner (vs. the generic reconnect spinner) would save a
    lot of confused debugging.

## What worked well (keep)

- **Concierge → workspace delegation** via the `ask` tool, with a clear "what will happen" summary.
- **Reviewable planning**: decomposition into a sensible dependency-chained DAG, parked for approval.
- **Refine loop genuinely works**: Critic produced substantive, spec-checked reviews; Forge
  **incorporated** feedback — it applied the Critic's `_ => continue` → `_ => return None` fix
  automatically, and it applied my *human* change-request (added a BFS `is_reachable_from` +
  `stairs_reachable_from_spawn` test over 8 seeds, fixed a misleading comment, and stayed scoped to
  `map.rs` exactly as asked — all 5 tests green).
- **Run-chain transparency** (Activity tab): every step expands to its tool calls (`read`/`bash
  cargo test`/`edit`/`write`) with coloured diffs and pass/revise chips — excellent for review.
- **Human review actions** (Approve / Reject / Request-changes with a free-text box) are available
  in-place over the board via the Peek panel, and a human "request changes" correctly re-opens the
  task as a `revise` run carrying the feedback.
