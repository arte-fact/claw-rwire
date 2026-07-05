//! Blocking `bash` execution for the coder toolbox. Runs on the worker thread (no
//! tokio), with `cwd` pinned to the agent's workspace and a hard wall-clock timeout.
//!
//! Output is redirected to temp files rather than pipes: polling `try_wait` while a child
//! writes to a full pipe buffer would deadlock (the child blocks on write, never exits, so
//! the poll never sees it finish). Files have no such backpressure.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Per-command wall-clock cap (matches rust-claw's `BASH_TIMEOUT`).
pub const BASH_TIMEOUT: Duration = Duration::from_secs(120);
/// Cap on captured output fed back to the model, so a chatty command can't bloat the
/// transcript (and the DOM). Excess is truncated with a note.
const MAX_OUTPUT: usize = 32_000;
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Where a `bash` command runs (R6). The host dir is always the workspace; `Isola` runs the
/// command inside that workspace's sandbox (the dir is bind-mounted, so file writes are shared).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecStrategy {
    /// Spawn `bash` directly on the host in `root` (the pre-R6 path).
    Host { root: PathBuf },
    /// Run `bash` inside the named isola sandbox; `mount` is the host workspace dir bind-mounted in.
    Isola { sandbox: String, mount: PathBuf },
}

/// Run `command` through the chosen [`ExecStrategy`]: combined output + exit, workspace cwd, and
/// the [`BASH_TIMEOUT`] wall-clock cap. Returns `exit <code>\n<output>` (stdout then stderr); on
/// timeout the command is killed and a note returned.
#[must_use]
pub fn bash_in(strategy: &ExecStrategy, command: &str) -> String {
    run_in(strategy, command, BASH_TIMEOUT)
}

/// [`bash_in`] with a caller-chosen wall-clock cap — the machine-check and CI runners need a
/// longer leash than interactive `bash` (P0: every harness-run command goes through the
/// strategy; nothing calls the host path directly when the lane is sandboxed).
#[must_use]
pub fn run_in(strategy: &ExecStrategy, command: &str, timeout: Duration) -> String {
    match strategy {
        ExecStrategy::Host { root } => run(root, command, timeout),
        ExecStrategy::Isola { sandbox, mount } => run_isola(sandbox, mount, command, timeout),
    }
}

pub(crate) fn run(workspace: &Path, command: &str, timeout: Duration) -> String {
    let (out_path, err_path) = temp_paths();
    let (out_file, err_file) = match (
        std::fs::File::create(&out_path),
        std::fs::File::create(&err_path),
    ) {
        (Ok(o), Ok(e)) => (o, e),
        _ => return "error: bash could not create its output buffers".to_owned(),
    };

    let spawned = Command::new("bash")
        .arg("-c")
        .arg(command)
        .current_dir(workspace)
        // Credential hygiene: agent git must never prompt or borrow the host user's
        // identity — no global config (aliases, credential helpers), no ssh-agent, no
        // interactive auth. Public clones/fetches still work; pushes can't ride ambient
        // credentials even if a write slips past the git screen via a subshell.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes")
        .env_remove("SSH_AUTH_SOCK")
        .stdin(Stdio::null())
        .stdout(out_file)
        .stderr(err_file)
        .spawn();

    let mut child = match spawned {
        Ok(child) => child,
        Err(err) => {
            cleanup(&out_path, &err_path);
            return format!("error: failed to start bash: {err}");
        }
    };

    let start = Instant::now();
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break format_result(status.code(), &out_path, &err_path),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let output = read_combined(&out_path, &err_path);
                    break format!(
                        "timed out after {}s — command killed\n{output}",
                        timeout.as_secs()
                    );
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(err) => {
                let _ = child.kill();
                break format!("error: waiting on bash failed: {err}");
            }
        }
    };

    cleanup(&out_path, &err_path);
    result
}

fn format_result(code: Option<i32>, out_path: &Path, err_path: &Path) -> String {
    let output = read_combined(out_path, err_path);
    let code = code.map_or_else(|| "killed".to_owned(), |c| c.to_string());
    if output.is_empty() {
        format!("exit {code}")
    } else {
        format!("exit {code}\n{output}")
    }
}

/// stdout then stderr, each lossy-decoded, joined, and truncated to [`MAX_OUTPUT`].
fn read_combined(out_path: &Path, err_path: &Path) -> String {
    let out = read_lossy(out_path);
    let err = read_lossy(err_path);
    let mut combined = out;
    if !err.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&err);
    }
    let trimmed = combined.trim_end();
    if trimmed.chars().count() > MAX_OUTPUT {
        let head: String = trimmed.chars().take(MAX_OUTPUT).collect();
        format!("{head}\n… (output truncated)")
    } else {
        trimmed.to_owned()
    }
}

fn read_lossy(path: &Path) -> String {
    std::fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

/// Unique temp paths for one command's stdout/stderr (pid + a process-wide counter, so
/// concurrent runs on different workers never collide).
fn temp_paths() -> (std::path::PathBuf, std::path::PathBuf) {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let dir = std::env::temp_dir();
    (
        dir.join(format!("claw-bash-{pid}-{n}.out")),
        dir.join(format!("claw-bash-{pid}-{n}.err")),
    )
}

fn cleanup(out_path: &Path, err_path: &Path) {
    let _ = std::fs::remove_file(out_path);
    let _ = std::fs::remove_file(err_path);
}

/// Run `command` inside the lane's isola sandbox as the sandbox user, returning the same
/// `exit <code>\n<output>` shape as the host path. CLI-driven (`isola exec` subprocess): exit
/// codes propagate, output is captured from the pipes, and the wall-clock cap is enforced
/// *inside* the sandbox with `timeout` (exit 124 → note) — no host-side child to kill. The
/// sandbox is created on demand with the minimal agent env set; first provision takes minutes
/// (cached after), billed to the first `bash` of the lane.
fn run_isola(sandbox: &str, mount: &Path, command: &str, timeout: Duration) -> String {
    if let Err(err) = crate::sandbox::ensure(sandbox, mount) {
        return format!("error: sandbox {sandbox:?} unavailable: {err}; command not run");
    }
    let secs = timeout.as_secs().max(1);
    // `timeout -k 5` sends TERM at the cap, then KILL 5s later if the command ignores it.
    let wrapped = format!("timeout -k 5 {secs} bash -lc {}", single_quote(command));
    let output = Command::new("isola")
        .arg("exec")
        .arg("-w")
        .arg(mount)
        .arg(sandbox)
        .args(["/bin/bash", "-lc", &wrapped])
        .stdin(Stdio::null())
        .output();
    match output {
        Ok(out) => {
            let code = out.status.code().unwrap_or(-1);
            let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&out.stderr);
            if !stderr.trim().is_empty() {
                if !combined.is_empty() && !combined.ends_with('\n') {
                    combined.push('\n');
                }
                combined.push_str(stderr.trim_end());
            }
            let body = truncate_output(&combined);
            if code == 124 {
                if body.is_empty() {
                    format!("timed out after {secs}s — command killed")
                } else {
                    format!("timed out after {secs}s — command killed\n{body}")
                }
            } else if body.is_empty() {
                format!("exit {code}")
            } else {
                format!("exit {code}\n{body}")
            }
        }
        Err(err) => format!("error: sandbox exec failed: {err}"),
    }
}

/// Cap captured output the same way the host path does (the host path truncates as it reads from
/// files; the subprocess hands back one string, so cap it here).
fn truncate_output(output: &str) -> String {
    let trimmed = output.trim_end();
    if trimmed.chars().count() > MAX_OUTPUT {
        let head: String = trimmed.chars().take(MAX_OUTPUT).collect();
        format!("{head}\n… (output truncated)")
    } else {
        trimmed.to_owned()
    }
}

/// POSIX single-quote a string for safe embedding in a `bash -lc` payload: wrap in `'…'`,
/// rewriting any embedded `'` as `'\\''`.
fn single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("claw-exec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn bash(workspace: &Path, command: &str) -> String {
        run(workspace, command, BASH_TIMEOUT)
    }

    #[test]
    fn captures_stdout_and_exit_code() {
        let out = bash(&ws(), "echo hello");
        assert_eq!(out, "exit 0\nhello");
    }

    #[test]
    fn reports_nonzero_exit_and_stderr() {
        let out = bash(&ws(), "echo oops >&2; exit 3");
        assert_eq!(out, "exit 3\noops");
    }

    #[test]
    fn runs_in_the_workspace_cwd() {
        let dir = ws();
        let out = bash(&dir, "pwd");
        let real = dir.canonicalize().expect("canon");
        assert_eq!(out.trim(), format!("exit 0\n{}", real.display()));
    }

    #[test]
    fn exec_strategy_host_runs_on_the_host() {
        let strategy = ExecStrategy::Host { root: ws() };
        assert_eq!(bash_in(&strategy, "echo hello"), "exit 0\nhello");
    }

    #[test]
    fn bash_env_is_git_hermetic() {
        // No prompts, no global config (credential helpers/aliases), no ssh-agent — a git
        // write that slips past the screen via a subshell still finds no ambient identity.
        let out = bash(
            &ws(),
            "echo p=$GIT_TERMINAL_PROMPT g=$GIT_CONFIG_GLOBAL s=${SSH_AUTH_SOCK:-unset}",
        );
        assert_eq!(out, "exit 0\np=0 g=/dev/null s=unset");
    }

    #[test]
    fn kills_a_command_that_exceeds_the_timeout() {
        // A 10s sleep against a 1s cap must be killed promptly, not waited out.
        let start = Instant::now();
        let out = run(&ws(), "sleep 10", Duration::from_secs(1));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "should kill early"
        );
        assert!(out.starts_with("timed out after 1s"), "got: {out}");
    }

    // Gated integration test (T6/P0). Needs a host with `isola setup-host` run and a sandbox
    // named by CLAW_TEST_SANDBOX whose mount is CLAW_TEST_MOUNT. Run with:
    //   CLAW_TEST_SANDBOX=it CLAW_TEST_MOUNT=/tmp/claw-it cargo test -- --ignored isola_
    #[test]
    #[ignore = "requires a provisioned isola host + CLAW_TEST_SANDBOX/CLAW_TEST_MOUNT"]
    fn isola_runs_in_the_sandbox_isolates_writes_and_reaps() {
        let sandbox = std::env::var("CLAW_TEST_SANDBOX").expect("set CLAW_TEST_SANDBOX");
        let mount = PathBuf::from(std::env::var("CLAW_TEST_MOUNT").expect("set CLAW_TEST_MOUNT"));
        std::fs::create_dir_all(&mount).expect("mkdir mount");
        let strategy = ExecStrategy::Isola {
            sandbox,
            mount: mount.clone(),
        };

        // A write into the bind-mounted workspace is visible on the host.
        let out = bash_in(&strategy, "echo hi > marker.txt && cat marker.txt");
        assert!(out.contains("hi"), "expected command output, got: {out}");
        assert!(
            mount.join("marker.txt").exists(),
            "a workspace write should be visible on the host"
        );

        // A stray write outside the workspace must NOT reach the real host home.
        let home = std::env::var("HOME").expect("HOME");
        let probe = Path::new(&home).join("claw-escape-probe");
        let _ = std::fs::remove_file(&probe);
        let _ = bash_in(
            &strategy,
            "echo pwned > \"$HOME/claw-escape-probe\" || true",
        );
        assert!(
            !probe.exists(),
            "a $HOME write inside the sandbox must not escape to the host"
        );

        // The host filesystem simply isn't there: an absolute host path (the symlink
        // episode's read probe) resolves to nothing inside the mount namespace.
        let read = bash_in(&strategy, "ls /home/artefact 2>&1; true");
        assert!(
            read.contains("No such file") || read.contains("cannot access"),
            "the host home must be invisible inside the sandbox, got: {read}"
        );

        // The wall-clock cap reaps an over-running command (in-sandbox `timeout`).
        let timed = run_isola(
            &match &strategy {
                ExecStrategy::Isola { sandbox, .. } => sandbox.clone(),
                ExecStrategy::Host { .. } => unreachable!(),
            },
            &mount,
            "sleep 30",
            Duration::from_secs(1),
        );
        assert!(
            timed.starts_with("timed out after 1s"),
            "expected a timeout note, got: {timed}"
        );
    }
}
