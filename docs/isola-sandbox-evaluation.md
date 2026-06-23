# Spike: `arte-fact/isola` as workspace sandbox isolation

Evaluation for [improvement request R6](improvement-requests.md#r6--spike-evaluate-arte-factisola-as-workspace-sandbox-isolation).
Verdict up front: **a strong fit for the *host-safety* goal** (sandbox the agent's shell so it
can't escape the workspace), worth adopting behind a flag. It is **not, by itself, the
in-workspace-parallelism unlock** the task board defers — that still needs per-run worktrees, which
compose *with* isola. Source read at `github.com/arte-fact/isola@main` (public).

## What isola is

A Rust CLI + engine (`isola` binary over an `isola-core` crate) that gives each project a
**persistent, throwaway Ubuntu 24.04 sandbox**, rootless and daemonless.

| Property | Detail |
|---|---|
| Mechanism (Linux) | **User namespaces** + PID/mount/UTS/IPC namespaces + **seccomp** syscall filtering (`isola-core/src/sandbox/linux/{namespace,userns,mounts,seccomp}.rs`). No root, no daemon. |
| Mechanism (macOS) | A **Lima** micro-VM backend. |
| Persistence | Per-name rootfs at `~/.isola/sandboxes/<name>/rootfs/`; survives across sessions; shared download/rootfs caches make recreate near-instant. |
| Workspace | Host project dir **bind-mounted live, two-way** at `/<dirname>`; sandbox uid 1000 maps back to the host user, so files stay owned by you. |
| Network | **Shared with the host — not isolated** by default. |
| Interface | CLI (`create`/`enter`/`exec`/`destroy`/`list`/`status`/`reprovision`) **and** a library: `SandboxBackend::exec_command(name, &cmd, workspace, devices) -> Result<i32>`. |
| Provisioning | A **plugin** system (`plugin.yaml` + `install.sh`): toolchains (`rust`/`nodejs`/`python-uv`/`go`/`php`), tools, GPU (`cuda`/`rocm`/`gpu`), host-config imports, shells. |
| License / lang | **Apache-2.0**, ~310 KB Rust + a little shell. CI (`rust.yml`) + integration tests (`isolation.rs`, `lifecycle.rs`). |
| Maturity | Young, single maintainer — **the same author as this project** (`arte-fact`), so coordination/upstreaming cost is ~zero. |

Note: it's literally described as "Claude code isolation" and ships a `claude` plugin — the design
target is sandboxing AI coding agents, which is exactly claw's "let an agent loose safely" need.

## Fit for claw workspaces

claw already has the right shape for this:

- **One sandbox per workspace.** Name = workspace id, mount = `<data>/workspaces/<id>`. Persistence
  matches claw's model (a workspace runs tasks **sequentially**, reusing its dir).
- **Only the shell needs sandboxing.** Because the workspace is bind-mounted two-way, claw's file
  tools (`write`/`edit`/`read`, which operate on the host dir) keep working unchanged and stay
  host-side. **Just the `bash`/exec tool** (`src/tools/exec.rs`, `src/tools/coder.rs`) routes into
  the sandbox. That's the dangerous surface (arbitrary commands) and the cheap one to redirect.
- **The exec tool already captures stdout + exit + enforces a timeout** (`src/tools/exec.rs` tests:
  `captures_stdout_and_exit_code`, `runs_in_the_workspace_cwd`, `kills_a_command_that_exceeds_the_timeout`).
  isola has a captured path internally (`run_command_captured`, `capture_output: true`) — see the
  gap below.

What it **does** isolate: filesystem (host home/system untouched), process tree, syscalls (seccomp).
What it **does not**: the network (shared with host) — so it bounds file/process blast radius, not
exfiltration.

## Does it unlock in-workspace parallelism?

**No, not on its own.** The task board defers "parallelism within a workspace (needs run isolation
— subdir / worktree copy)" precisely to avoid concurrent writes colliding in the shared dir. Two
isola sandboxes for the same workspace both **bind-mount the same host dir**, so they'd still
collide. isola solves *host* isolation, not *inter-run* isolation.

They **compose**, though: give each concurrent run its own **git worktree** (or copy) and bind-mount
*that* into its own sandbox → isolation + parallelism. So isola is complementary to, not a
substitute for, the worktree work.

## Integration sketch

Prefer the **library** path (both are Rust, Apache-2.0, same author) over shelling out to the binary:

1. **Dependency.** Add `isola-core` as a path/git dep (gated to `cfg(target_os = "linux")`, behind a
   `sandbox` cargo feature).
2. **Execution backend.** In `src/tools/exec.rs`, add an isola backend: instead of spawning the
   command in `workspace_dir` on the host, run it inside the workspace's sandbox. File tools are
   untouched.
3. **Lifecycle hooks** (in `src/bridge/admin.rs` / `src/workspace/`):
   - workspace create / first run → ensure the sandbox exists (`create`, idempotent), mount =
     workspace dir, plugins from a per-workspace config (default: a minimal base + the workspace's
     toolchain).
   - workspace delete (`DeleteWorkspace`) → `destroy`.
4. **Opt-in + fallback.** A `CLAW_SANDBOX=isola` env / setting selects the backend; absent or on a
   host without the one-time setup, fall back to today's direct execution. Keep the echo/no-endpoint
   and unit-test paths on the direct backend.
5. **Keep sandboxes long-lived.** `exec` into a persistent sandbox per call — never create/destroy
   per run — so the per-command cost is namespace entry, not rootfs provisioning.

### The one real code gap

`exec_command` (run **as the sandbox user**) inherits stdio for interactive `isola exec`; the
**captured** path (`run_command_captured`, `capture_output: true`) currently serves provisioning
(root). claw needs **captured stdout/stderr *as the sandbox user*** to feed the model. That's a
small addition to `isola-core` (a captured + as-sandbox-user variant) — trivial since we own isola.

## Risks / costs

- **One-time host setup with `sudo`** (Ubuntu 24.04+: AppArmor profile + `/etc/subuid`). Fine for a
  self-hosted server (this user's env is Linux); friction for any future distribution.
- **Disk per workspace** — a full Ubuntu rootfs each (mitigated by shared caches). Many workspaces =
  real disk; may want a shared base + per-workspace overlay, or sandbox-on-demand.
- **Per-command latency** — namespace entry per `bash` call. Acceptable if sandboxes stay warm;
  measure before committing.
- **Network not isolated** — agents still reach the internet freely. If network containment is ever
  wanted, isola won't provide it as-is.
- **Captured-as-sandbox-user** stdio (above) — needs the small upstream add.
- **Maturity** — young, solo. Offset by: Apache-2.0, tested, and authored by this project's owner.
- **Linux-first** — claw's server target is Linux ✓; macOS dev would pull in Lima (heavier), but
  claw is web/server-first so that's a non-issue.

## Alternatives (for context)

| Option | Host safety | In-workspace parallelism | Cost / notes |
|---|---|---|---|
| **isola** | ✅ userns + seccomp, rootless | ➖ (needs worktrees too) | one-time sudo setup; disk per rootfs; **same author** |
| git **worktree** per run | ❌ none | ✅ separate trees | cheap; solves collisions, not host safety |
| **OverlayFS** per run | ❌ | ✅ copy-on-write | needs mount privs; no process/syscall isolation |
| `bwrap` (bubblewrap) | ✅ similar userns | ➖ | a dependency, not a managed-sandbox lifecycle |
| **podman/docker** | ✅ strong | ✅ | daemon/root or rootless-podman setup; heavier; not rootless-by-default like isola |
| microVM (Firecracker) | ✅✅ strongest | ✅ | far heavier; overkill here |

isola sits at the sweet spot for this project: rootless host-safety with a managed,
persistent, plugin-provisioned lifecycle — and zero coordination cost to extend.

## Recommendation

**Go — adopt incrementally, behind a flag.**

- **Phase 0 (spike → small upstream):** add the captured-stdout-as-sandbox-user variant to
  `isola-core`; benchmark `exec` latency on a warm sandbox vs direct.
- **Phase 1 (safety):** one persistent sandbox per workspace; route only the `bash`/exec tool
  through it; file tools stay host-side; opt-in via `CLAW_SANDBOX=isola` with direct-execution
  fallback. This delivers the "safe for agents" win with minimal surface change.
- **Phase 2 (optional, parallelism):** per-run **worktree + sandbox** to lift the
  one-task-at-a-time-per-workspace limit — the deferred task-board goal — once Phase 1 is proven.

Net: isola is the right tool for sandboxing claw's agent execution, it's permissively licensed and
library-friendly, and because we own it the only real gap (captured stdio as the sandbox user) is a
quick fix rather than a blocker.
