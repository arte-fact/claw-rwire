//! Sandbox mode + lifecycle for agent command execution (R6/P0). `Host` runs `bash` directly
//! in the workspace dir; `Isola` runs it inside a per-lane rootless [isola] sandbox so a
//! stray command can't touch the host beyond the bind-mounted lane.
//!
//! Integration is **CLI-driven** (`isola create/status/exec/destroy` as subprocesses): the
//! CLI is isola's stable interface, exit codes propagate, and a warm exec costs ~15 ms.
//! Sandboxes are created on demand at first `bash` (worker thread — first provision takes
//! minutes, cached after) with [`AGENT_ENVS`] only: self-contained toolchains, never the
//! operator's user-layer configs (ssh-keys, claude-config, git-config).
//!
//! [isola]: first-party `../isola`

use std::path::Path;
use std::process::Command;

/// Where an agent's `bash` commands run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SandboxMode {
    /// Spawn `bash` on the host, in the workspace dir. The default and pre-R6 behaviour.
    #[default]
    Host,
    /// Run `bash` inside the lane's isola sandbox.
    Isola,
}

impl SandboxMode {
    /// Map a requested mode string (`CLAW_SANDBOX`) to a [`SandboxMode`]. `"isola"` (any casing,
    /// trimmed) selects [`SandboxMode::Isola`]; everything else — including `None` — is `Host`.
    #[must_use]
    pub fn resolve(requested: Option<&str>) -> Self {
        match requested.map(|value| value.trim().to_ascii_lowercase()) {
            Some(value) if value == "isola" => Self::Isola,
            _ => Self::Host,
        }
    }

    /// Whether isola-backed execution is available: Linux with the `isola` binary on PATH.
    /// Probed once per process. When false, a resolved [`SandboxMode::Isola`] must degrade
    /// to `Host` (the engine can't sandbox).
    #[must_use]
    pub fn isola_available() -> bool {
        static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *AVAILABLE.get_or_init(|| {
            cfg!(target_os = "linux")
                && Command::new("isola")
                    .arg("--help")
                    .output()
                    .is_ok_and(|out| out.status.success())
        })
    }

    /// The effective mode: the resolved mode, demoted to `Host` when isola isn't available.
    #[must_use]
    pub fn effective(self) -> Self {
        match self {
            Self::Isola if !Self::isola_available() => Self::Host,
            other => other,
        }
    }
}

/// Environments provisioned into agent sandboxes: self-contained toolchains installed into
/// the rootfs. Deliberately excludes every user-layer plugin — an agent sandbox must never
/// inherit the operator's ssh keys, API configs, or git identity.
const AGENT_ENVS: &[&str] = &["rust", "git", "nodejs"];

/// Sandbox name for a run rooted at `dir`, derived from the data-dir layout: a canonical
/// workspace (`…/workspaces/<ws>`) gets `claw-<ws>`; a request lane (`…/trees/<ws>/<req8>`)
/// gets `claw-<ws>-<req8>` — its own sandbox, so lane teardown kills lane processes without
/// touching the workspace's. Anything else falls back to the final path component.
#[must_use]
pub fn name_for(dir: &Path) -> String {
    let comps: Vec<&str> = dir
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    if let Some(i) = comps.iter().rposition(|c| *c == "trees")
        && comps.len() > i + 2
    {
        return format!("claw-{}-{}", comps[i + 1], comps[i + 2]);
    }
    if let Some(i) = comps.iter().rposition(|c| *c == "workspaces")
        && comps.len() > i + 1
    {
        return format!("claw-{}", comps[i + 1]);
    }
    format!(
        "claw-{}",
        comps
            .last()
            .copied()
            .unwrap_or("workspace")
            .trim_matches('/')
    )
}

/// Per-name creation lock: a background warm and a worker's first `bash` may race to
/// create the same sandbox — the loser must wait for the winner, not collide with it
/// (a concurrent create fails "already exists" and the sweep would destroy the one in
/// progress).
fn creation_lock(name: &str) -> std::sync::Arc<std::sync::Mutex<()>> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<std::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    let mut map = LOCKS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.entry(name.to_owned()).or_default().clone()
}

/// Ensure `name` exists, creating it with the minimal agent env set on first use. Creation
/// provisions a rootfs (minutes — call from worker/background threads only); racing callers
/// serialize per name, so a second caller just waits and finds the sandbox ready.
pub fn ensure(name: &str, mount: &Path) -> Result<(), String> {
    let lock = creation_lock(name);
    let _guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if exists(name) {
        return Ok(());
    }
    // Unhealthy-but-present (a create that died partway) blocks re-creation — sweep it.
    // Destroy is idempotent; a never-created sandbox just errors quietly.
    destroy(name);
    let mut cmd = Command::new("isola");
    // The snapshot cache (isola, reworked 2026-07-05) makes this ~20 s after the first
    // full provision of the env combo; a corrupt cache self-heals upstream (sanity-check,
    // discard, fall through to full provisioning).
    cmd.arg("create").arg("-w").arg(mount);
    for env in AGENT_ENVS {
        cmd.args(["--env", env]);
    }
    cmd.arg(name);
    match cmd.output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let tail: String = stderr.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
            Err(format!("isola create failed: {tail}"))
        }
        Err(err) => Err(format!("isola create: {err}")),
    }
}

/// If `name` doesn't exist yet, start provisioning it on a background thread and return
/// `true` — the caller informs the user, the provisioning overlaps the run's first LLM
/// turn, and the first `bash` (whose [`ensure`] serializes on the same per-name lock)
/// finds it ready or waits out the remainder instead of the full build.
pub fn warm(name: &str, mount: &Path, log: impl Fn(String) + Send + 'static) -> bool {
    if !SandboxMode::isola_available() || exists(name) {
        return false;
    }
    let (name, mount) = (name.to_owned(), mount.to_path_buf());
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        match ensure(&name, &mount) {
            Ok(()) => log(format!(
                "sandbox: {name} provisioned in {}s",
                started.elapsed().as_secs()
            )),
            Err(err) => log(format!("sandbox: warming {name} failed: {err}")),
        }
    });
    true
}

/// Whether the sandbox exists and is healthy (`isola status` exits 0).
#[must_use]
pub fn exists(name: &str) -> bool {
    Command::new("isola")
        .args(["status", name])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Best-effort destroy: kills everything running inside and deletes the rootfs. Idempotent —
/// a missing sandbox (never created, or already torn down) is fine. The teardown seam for
/// lanes (deliver/abandon) and workspaces (admin delete): no orphaned writer survives it.
pub fn destroy(name: &str) {
    let _ = Command::new("isola").args(["destroy", name]).output();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_host() {
        assert_eq!(SandboxMode::resolve(None), SandboxMode::Host);
        assert_eq!(SandboxMode::default(), SandboxMode::Host);
    }

    #[test]
    fn reads_isola_from_env() {
        assert_eq!(SandboxMode::resolve(Some("isola")), SandboxMode::Isola);
        assert_eq!(SandboxMode::resolve(Some("  ISOLA ")), SandboxMode::Isola);
    }

    #[test]
    fn unknown_value_is_host() {
        assert_eq!(SandboxMode::resolve(Some("host")), SandboxMode::Host);
        assert_eq!(SandboxMode::resolve(Some("")), SandboxMode::Host);
        assert_eq!(SandboxMode::resolve(Some("docker")), SandboxMode::Host);
    }

    #[test]
    fn host_stays_host() {
        assert_eq!(SandboxMode::Host.effective(), SandboxMode::Host);
    }

    #[test]
    fn names_canonical_workspaces_and_lanes_apart() {
        let ws = Path::new("/data/claw-data/workspaces/studio");
        assert_eq!(name_for(ws), "claw-studio");
        let lane = Path::new("/data/claw-data/trees/studio/ab12cd34");
        assert_eq!(name_for(lane), "claw-studio-ab12cd34");
        // A lane's project subdir still resolves to the lane, not the project.
        let sub = Path::new("/data/claw-data/trees/studio/ab12cd34/projects/empire");
        assert_eq!(name_for(sub), "claw-studio-ab12cd34");
        let odd = Path::new("/somewhere/else");
        assert_eq!(name_for(odd), "claw-else");
    }
}
