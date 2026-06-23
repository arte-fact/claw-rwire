# Implementation roadmap — sandboxing claw execution with isola

> **Progress (M5).**
> - **P0/U1 — DONE** (isola repo, branch `feat/exec-command-captured`, rev `d1a3c8b`): isola now
>   builds a **library** target (was binary-only) so claw can embed it, and `SandboxBackend` gained
>   **`exec_command_captured`** — runs as the sandbox user and returns `(exit, combined stdout+stderr)`
>   (Linux drains the capture pipe before reaping; Lima captures `limactl` output). isola gate green.
> - **P1/T1 — DONE** (claw): optional `isola` dep (`../isola`, Linux-only) behind a `sandbox`
>   feature, off by default. (Sonatype check: `isola` is a first-party path dep, not a published
>   package, so the registry check is N/A; its transitive deps are standard — `nix`/`libc`/`clap`.)
> - **P1/T2 — DONE** (claw): `src/sandbox.rs` `SandboxMode {Host, Isola}` + `resolve` /
>   `isola_available` / `effective`; the bridge logs the mode at startup and warns+falls-back when
>   isola is requested but not compiled in. 4 tests; gate green with the feature on **and** off.
> - **P1/T3 — DONE** (claw): `exec::ExecStrategy {Host, Isola}` + `bash_in`; `run_isola` execs via
>   `exec_command_captured`. The wall-clock cap is enforced with **in-sandbox `timeout`** (exit 124
>   → timeout note), which sidesteps the host-side-reap problem entirely. `CoderToolbox` carries the
>   strategy; file tools still use the host workspace dir.
> - **P1/T4 — DONE (with one deviation)** (claw): `build_toolbox` picks the strategy from
>   `SandboxMode` (`exec_strategy_for`: sandbox name = workspace id, mount = workspace dir).
>   **Deviation:** an unhealthy/missing sandbox is *reported* ("create it first"), not lazily
>   auto-created — full inline provisioning (rootfs extract + plugin provision) was out of scope for
>   this pass; create out-of-band with `isola create <workspace-id>`. Lazy auto-provision is a
>   follow-up.
> - **P1/T5 — DONE** (claw): `admin::delete_workspace` destroys the workspace's sandbox first
>   (best-effort, feature-gated, idempotent) so rootfs dirs don't leak.
> - **P1/T6 — DONE (compiles; host-gated)** (claw): an `#[ignore]` integration test asserts the
>   safety contract (workspace write visible on host; stray `$HOME` write does **not** escape;
>   in-sandbox timeout reaps). Compiles under `--features sandbox`; **runs only on a provisioned
>   host** via `CLAW_TEST_SANDBOX=… CLAW_TEST_MOUNT=… cargo test --features sandbox -- --ignored`.
> - **TODO:** **P0/U2** (warm-exec benchmark) and the **lazy auto-provision** deviation above; both
>   need a host with `isola setup-host` run (AppArmor + `/etc/subuid`, one-time `sudo`), which the
>   dev sandbox lacks. Live end-to-end verification (a real task's `bash` containing a stray write)
>   likewise awaits a provisioned host.


Turns the [isola spike](isola-sandbox-evaluation.md) verdict into a phased, PR-sized plan. Goal:
run a workspace's **agent `bash` commands inside an isola sandbox** so the agent can't touch the
host beyond its workspace — opt-in, with today's direct execution as the fallback. File tools stay
host-side (the workspace is bind-mounted), so only command execution changes.

Every subtask is independently green on the gate before the next:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Rules in force: no `#[allow]`/`unsafe`, enums over strings, `thiserror` in modules, ~300-line file
soft cap (split before adding), regression test per behaviour.

## Design at a glance

- **What changes:** only the `bash`/exec tool. `src/tools/exec.rs` gains an execution backend —
  `Host` (today: spawn in the workspace dir) or `Isola` (run inside the workspace's sandbox).
- **What doesn't:** `write`/`edit`/`read` keep operating on the host workspace dir, which isola
  bind-mounts two-way into the sandbox — so host-side writes are instantly visible inside and vice
  versa. No change to the file tools, the scheduler, or the run model.
- **Sandbox identity:** one persistent sandbox per workspace, name = workspace id, mount =
  `<data>/workspaces/<id>` (`workspace::dir_for`). Reused across runs (matches claw's sequential
  lane).
- **Opt-in:** `CLAW_SANDBOX=isola` (read like the other `CLAW_*` envs in `bridge/mod.rs`); unset →
  `Host`. On a host without isola's one-time setup, log once and fall back to `Host` rather than
  failing runs.
- **Library, not CLI:** depend on `isola-core` (Apache-2.0, same author) and call
  `SandboxBackend::exec_command*` directly — no shelling out.

```
 claw task run ─► CoderToolbox(workspace_dir, ExecStrategy)
                    ├─ write/edit/read ─► host fs at <data>/workspaces/<id>   (unchanged)
                    └─ bash ─► ExecStrategy::Isola{sandbox=<id>}
                                 └─ isola_core exec_command_captured(<id>, cmd, mount, devices)
                                      └─ userns + seccomp sandbox, /<id> ← bind-mount (same files)
```

---

## P0 — Upstream prep + measurement (isola repo + a claw spike branch)

Small, blocking groundwork. Lives mostly in the **isola** repo (we own it).

### U1 — `exec_command_captured` as the sandbox user
**Goal.** claw needs the command's **combined stdout/stderr + exit code** to feed the model;
`exec_command` today runs as the sandbox user but **inherits** stdio (returns only `i32`), while the
captured path (`run_command_captured`, `capture_output: true`) runs during provisioning (root).

**Change (in isola-core).** Add a captured variant that runs **as the sandbox user** and returns
`(i32, String)` — reuse the `capture_output: true` plumbing in
`sandbox/linux/namespace::spawn_sandbox` and the Lima path; expose it on `SandboxBackend`
(`sandbox/backend.rs`) alongside `exec_command`. Unit-test in isola (`tests/isolation.rs` style).

**Done when.** `backend.exec_command_captured(name, &cmd, workspace, devices)` returns the command's
output + exit, as uid 1000, on both backends.

### U2 — Latency benchmark
**Goal.** Decide whether per-command sandbox entry is acceptable, and confirm "keep sandboxes warm"
(never create/destroy per run).

**Change.** A small bench: create a sandbox once, then time N `exec_command_captured` calls vs N
direct `std::process::Command` spawns of the same trivial command. Record p50/p95.

**Done when.** Numbers are written into [isola-sandbox-evaluation.md](isola-sandbox-evaluation.md);
warm-exec overhead is known and judged acceptable (or a mitigation is chosen).

---

## P1 — Host-safety: sandbox the `bash` tool (claw, behind a flag)

The core deliverable. After this, `CLAW_SANDBOX=isola` makes agent shell commands run in the
workspace's sandbox; everything else is unchanged.

### T1 — Add the dependency
**Goal.** `isola-core` available on Linux, behind a feature.

**Change.** `Cargo.toml`: a `sandbox` feature; `isola-core` as a git dep under
`[target.'cfg(target_os = "linux")'.dependencies]`, `optional = true`. **Run the Sonatype guide
dependency check first** (repo policy before adding any dep) — vuln/license/trust.

**Done when.** `cargo build` and `cargo build --features sandbox` both compile.

### T2 — Resolve the sandbox mode
**Goal.** A single source of truth for `Host` vs `Isola`.

**Change.** `enum SandboxMode { Host, Isola }` (new small module, e.g. `src/sandbox.rs`), resolved
from `CLAW_SANDBOX` via the existing `env()` helper in `bridge/mod.rs`. Default `Host`. When
`isola` is requested but the `sandbox` feature is off or the platform isn't Linux, resolve to
`Host` and `log_line` once.

**Tests.** `sandbox_mode_defaults_to_host`; `sandbox_mode_reads_isola_from_env`.

### T3 — Execution backend in the exec tool
**Goal.** Route `bash` through the chosen backend without changing its contract (combined output +
exit, workspace cwd, timeout). `src/tools/exec.rs` tests today:
`captures_stdout_and_exit_code`, `runs_in_the_workspace_cwd`, `kills_a_command_that_exceeds_the_timeout`.

**Change.**
- `enum ExecStrategy { Host { root: PathBuf }, Isola { sandbox: String, mount: PathBuf } }`.
- `CoderToolbox` carries an `ExecStrategy` (today it carries `root: PathBuf`); the file tools keep
  using `root`/`mount` (the host dir), only the `bash` arm branches.
- `Host` → today's `std::process::Command` in `root` (unchanged path).
- `Isola` → `isola_core` `exec_command_captured(sandbox, ["bash","-lc",cmd], Some(mount), devices)`,
  mapping `(exit, output)` into the same `ToolOutcome`.
- **Timeout:** run the captured call on a worker thread with the existing timeout; on expiry, kill
  the host-side isola child — its PID-namespace init dying tears down the sandboxed process tree.
  Verify this actually reaps (integration test in T6).

**Tests.** Keep all existing exec tests green (they exercise `Host`). Add
`exec_strategy_host_runs_on_the_host` (explicit).

### T4 — Build the toolbox with the strategy + lazy ensure-sandbox
**Goal.** Wire the mode through to the toolbox, and make the sandbox exist on demand.

**Change.**
- `bridge::build_toolbox(selection, workspace, workspace_default)` (`bridge/mod.rs`) picks the
  `ExecStrategy` from `SandboxMode`: `Isola` uses the workspace dir's final component as the sandbox
  name (= the workspace id) and the dir as the mount.
- First isola exec in a workspace: if `!backend.is_healthy(name)`, `isola_core` create (mount =
  workspace dir, plugins from a default set / `CLAW_SANDBOX_PLUGINS`). If create fails because the
  **host setup is missing** (AppArmor/subuid), `log_line` a clear "run `isola setup-host`" message
  and fall back to `Host` for that run — never crash.

**Tests.** Mode→strategy selection is unit-testable; the create/health path is covered by the gated
integration test (T6), since it needs a real host.

### T5 — Teardown on workspace delete
**Goal.** Don't leak rootfs dirs.

**Change.** `bridge::admin::delete_workspace` (after the dir removal it already does) → best-effort
`isola_core` `destroy(name)` when the feature is on. Idempotent / ignore "not found".

**Tests.** Logic guard only (destroy is a real-host op); covered by T6.

### T6 — Tests + gate
**Goal.** Prove safety without making the suite depend on a provisioned host.

**Change.**
- Unit: mode/strategy selection (pure).
- **Gated integration test** (`#[ignore]` by default, run via `cargo test -- --ignored` on a host
  with isola set up): create a sandbox, exec a command that writes a file in the workspace (visible
  on the host) **and** a command that tries to write to `$HOME` outside the workspace (must not
  affect the real host home), exec a sleeping command and confirm the timeout reaps it, then destroy.

**Done when.** Gate green with the feature off; the ignored integration test passes on a set-up
host; `CLAW_SANDBOX=isola` end-to-end: a task's `bash` writes land in the workspace, a stray write
outside it does not hit the host, flag-off behaviour is byte-for-byte today's.

---

## P2 — (optional) In-workspace parallelism via worktree + sandbox

Lifts the deferred "one task at a time per workspace" limit. Larger; **own design doc before
building.** Listed for completeness.

- **W1 — per-run isolated tree.** For each concurrent run, materialise a private tree: `git worktree
  add` for a git workspace, else a copy/OverlayFS. Mount *that* into a per-run sandbox (name =
  `<id>-<run>`), not the shared dir.
- **W2 — relax the lane.** The scheduler currently runs one task per workspace
  (`tasks::running_in` gate in `scheduler::tick`). Allow N concurrent runs over isolated trees, up
  to a cap.
- **W3 — reconcile.** Define how isolated trees merge back to the canonical workspace (commit/merge,
  or last-writer with conflict surfacing). **This is the hard part** and the reason it's deferred.
- **Done when.** Two tasks in one workspace run concurrently without clobbering, with a defined
  merge/reconcile strategy.

---

## Sequencing & risk

| Phase | Depends on | Risk |
|---|---|---|
| **P0/U1** | — | Low. Small isola addition; we own the repo. |
| **P0/U2** | U1 | Low. Pure measurement; informs warm-vs-cold decision. |
| **P1** | U1 | Medium. The timeout→reap behaviour (T3) and host-setup fallback (T4) are the sharp edges — both have explicit tests. |
| **P2** | P1 proven | High. Reconcile (W3) is a genuine design problem; gate it behind a separate doc. |

**Key risks & decisions**
- **Host setup is `sudo` + one-time** (AppArmor + `/etc/subuid` on Ubuntu 24.04+). P1 must degrade
  gracefully when absent (T4), not assume it. Document the `isola setup-host` prerequisite.
- **Disk per workspace** (a full rootfs each, shared caches help). If workspace count grows, revisit
  a shared base + per-workspace overlay before P2.
- **Network is not isolated** — P1 bounds filesystem/process blast radius only. Out of scope here; a
  separate effort if network containment is ever wanted.
- **Plugins / toolchains** — a command needing `cargo`/`node` requires that plugin provisioned in
  the sandbox. Default to a minimal base in P1; make the plugin set per-workspace config and surface
  "tool not found" clearly. Decide the default plugin set during T4.
- **Latency** — keep sandboxes warm (P0/U2 confirms); never create/destroy per run.
- **Crate coupling** — pin the `isola-core` git dep to a rev; U1 must land upstream before T3 can use
  `exec_command_captured`.
